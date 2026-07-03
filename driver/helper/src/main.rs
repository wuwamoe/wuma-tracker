//! WumaTracker driver helper.
//!
//! The only normal-operation component that opens `\\.\WumaDisplayService`. It
//! exposes exactly one IPC operation (get player location) over a named pipe
//! that only Administrators and the account that launched this helper may open.
//! It never forwards raw IOCTLs and never accepts a PID, offsets, pointer
//! chains, pattern bytes, base addresses, or target process names from a
//! client — the driver already hardcodes all of that.

#[path = "../../shared/ioctls.rs"]
mod ioctls;

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;

use winapi::shared::minwindef::{DWORD, FALSE};
use winapi::shared::winerror::ERROR_PIPE_CONNECTED;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::ioapiset::DeviceIoControl;
use winapi::um::namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe};
use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
use winapi::um::securitybaseapi::GetTokenInformation;
use winapi::um::winbase::{
    LocalFree, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::winnt::{HANDLE, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER};

use ioctls::{GetLocationResponse, DEVICE_NAME, IOCTL_GET_LOCATION};

const PIPE_NAME: &str = r"\\.\pipe\WumaTrackerHelper";
const REQ_GET_LOCATION: u32 = 1;

extern "system" {
    // Not exposed by the winapi crate's bound feature set; both are ordinary
    // advapi32.dll exports (sddl.h / winreg.h) used exactly as MSDN documents.
    fn ConvertSidToStringSidW(Sid: *mut winapi::ctypes::c_void, StringSid: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        StringSecurityDescriptor: *const u16,
        StringSDRevision: DWORD,
        SecurityDescriptor: *mut PSECURITY_DESCRIPTOR,
        SecurityDescriptorSize: *mut DWORD,
    ) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Verifies this binary's own Authenticode signature before doing anything
/// privileged. Release builds must fail closed; the check is skipped in debug
/// builds only so local iteration does not require a production signing cert.
#[cfg(not(debug_assertions))]
fn verify_self_signature() -> bool {
    use winapi::shared::guiddef::GUID;
    use winapi::shared::minwindef::LPVOID;
    use winapi::um::wintrust::{
        WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };
    use winapi::um::wintrust::WinVerifyTrust;

    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let exe_path_w = wide(&exe_path.to_string_lossy());

    unsafe {
        let mut file_info: WINTRUST_FILE_INFO = mem::zeroed();
        file_info.cbStruct = mem::size_of::<WINTRUST_FILE_INFO>() as DWORD;
        file_info.pcwszFilePath = exe_path_w.as_ptr();

        let mut data: WINTRUST_DATA = mem::zeroed();
        data.cbStruct = mem::size_of::<WINTRUST_DATA>() as DWORD;
        data.dwUIChoice = WTD_UI_NONE;
        data.fdwRevocationChecks = WTD_REVOKE_NONE;
        data.dwUnionChoice = WTD_CHOICE_FILE;
        *data.u.pFile_mut() = &mut file_info;
        data.dwStateAction = WTD_STATEACTION_VERIFY;

        let mut action_guid: GUID = winapi::um::softpub::WINTRUST_ACTION_GENERIC_VERIFY_V2;
        let status = WinVerifyTrust(
            null_mut(),
            &mut action_guid,
            &mut data as *mut _ as LPVOID,
        );

        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(null_mut(), &mut action_guid, &mut data as *mut _ as LPVOID);

        status == 0
    }
}

#[cfg(debug_assertions)]
fn verify_self_signature() -> bool {
    true
}

/// Returns the SID string of the account running this process, so the pipe
/// ACL can be scoped to exactly that identity plus Administrators.
fn current_user_sid_string() -> Option<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return None;
        }

        let mut needed: DWORD = 0;
        GetTokenInformation(
            token,
            winapi::um::winnt::TokenUser,
            null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            CloseHandle(token);
            return None;
        }

        let mut buf = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            winapi::um::winnt::TokenUser,
            buf.as_mut_ptr() as *mut _,
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == 0 {
            return None;
        }

        let token_user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid_str: *mut u16 = null_mut();
        if ConvertSidToStringSidW(token_user.User.Sid as *mut _, &mut sid_str) == 0 {
            return None;
        }
        let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
        LocalFree(sid_str as *mut _);
        Some(s)
    }
}

/// Builds a security descriptor granting only Administrators (BA) and the
/// caller's own account read/write access, denying everyone else, including
/// Everyone, Authenticated Users, and remote/anonymous connections.
fn build_pipe_security_attributes(sd_buf: &mut PSECURITY_DESCRIPTOR) -> Option<SECURITY_ATTRIBUTES> {
    let user_sid = current_user_sid_string()?;
    let sddl = format!("D:P(A;;GRGW;;;BA)(A;;GRGW;;;{})", user_sid);
    let sddl_w = wide(&sddl);

    unsafe {
        let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_w.as_ptr(),
            1, // SDDL_REVISION_1
            sd_buf,
            null_mut(),
        );
        if ok == 0 {
            return None;
        }
    }

    Some(SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
        lpSecurityDescriptor: *sd_buf,
        bInheritHandle: FALSE,
    })
}

fn open_driver() -> Option<HANDLE> {
    let path_w = wide(DEVICE_NAME);
    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            winapi::um::winnt::GENERIC_READ | winapi::um::winnt::GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        None
    } else {
        Some(handle)
    }
}

/// Sentinel `stage` values used only on the helper<->app pipe (never sent by
/// the driver itself), so the app can tell "driver not open" apart from
/// "driver open but target process not found" apart from a real chain-read
/// failure (driver-reported stage 1..N).
const STAGE_DRIVER_UNAVAILABLE: u8 = 0xFF;
const STAGE_TARGET_MISSING: u8 = 0xFE;

/// The single supported operation: ask the driver for the current player
/// location and return the raw response. No other request shape exists.
fn get_location(driver: Option<HANDLE>) -> GetLocationResponse {
    let mut resp = GetLocationResponse {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
        stage: STAGE_DRIVER_UNAVAILABLE,
        _pad: [0; 3],
    };
    let Some(driver) = driver else {
        return resp;
    };

    let mut returned: DWORD = 0;
    unsafe {
        DeviceIoControl(
            driver,
            IOCTL_GET_LOCATION,
            null_mut(),
            0,
            &mut resp as *mut _ as *mut _,
            mem::size_of::<GetLocationResponse>() as DWORD,
            &mut returned,
            null_mut(),
        );
    }
    // Buffered I/O copies the driver's output buffer back based on the bytes
    // it reported as written, regardless of the completion NTSTATUS. The
    // driver only writes zero bytes back when it could not find the target
    // process (STATUS_NOT_FOUND, returned before touching the buffer); every
    // other path — success or chain failure — writes the full response.
    if returned == 0 {
        resp.stage = STAGE_TARGET_MISSING;
    }
    resp
}

fn serve_client(pipe: HANDLE, driver: Option<HANDLE>) {
    let mut file = unsafe { <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(pipe as _) };

    let mut req = [0u8; 4];
    if file.read_exact(&mut req).is_err() {
        std::mem::forget(file);
        return;
    }
    let opcode = u32::from_le_bytes(req);

    if opcode == REQ_GET_LOCATION {
        let resp = get_location(driver);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &resp as *const _ as *const u8,
                mem::size_of::<GetLocationResponse>(),
            )
        };
        let _ = file.write_all(bytes);
    }
    // Any other opcode is silently ignored and the connection is dropped;
    // there is no raw IOCTL forwarding and no way to widen the request shape.

    std::mem::forget(file);
}

fn main() {
    if !verify_self_signature() {
        eprintln!("wuma-tracker-helper: self signature verification failed, refusing to start");
        std::process::exit(1);
    }

    // The driver may not be loaded yet when the helper starts (or may be
    // unloaded later), so the handle is opened lazily and re-tried per
    // connection rather than treated as a one-time startup requirement.
    let mut driver: Option<HANDLE> = open_driver();
    if driver.is_none() {
        eprintln!(
            "wuma-tracker-helper: {} not available yet, will retry per request",
            DEVICE_NAME
        );
    }

    let pipe_name_w = wide(PIPE_NAME);

    loop {
        if driver.is_none() {
            driver = open_driver();
        }


        let mut sd_buf: PSECURITY_DESCRIPTOR = null_mut();
        let mut sa = match build_pipe_security_attributes(&mut sd_buf) {
            Some(sa) => sa,
            None => {
                eprintln!("wuma-tracker-helper: failed to build pipe security descriptor");
                std::process::exit(1);
            }
        };

        let pipe = unsafe {
            CreateNamedPipeW(
                pipe_name_w.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                &mut sa,
            )
        };

        unsafe {
            LocalFree(sd_buf as *mut _);
        }

        if pipe == INVALID_HANDLE_VALUE {
            eprintln!("wuma-tracker-helper: failed to create named pipe");
            std::process::exit(1);
        }

        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) != 0 }
            || unsafe { winapi::um::errhandlingapi::GetLastError() } == ERROR_PIPE_CONNECTED;

        if connected {
            serve_client(pipe, driver);
        }

        unsafe {
            DisconnectNamedPipe(pipe);
            CloseHandle(pipe);
        }
    }
}
