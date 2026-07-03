/// 커널 드라이버 기반 프로세스 백엔드
///
/// WinProc(ReadProcessMemory)와 동일한 ProcessBackend trait을 구현한다.
/// native_collector.rs의 `use crate::win_proc::WinProc as PlatformProc;` 한 줄을
/// `use crate::win_proc_driver::WinProcDriver as PlatformProc;` 으로 바꾸면 교체 완료.
///
/// 이 앱은 드라이버를 직접 열지 않는다. 모든 좌표 조회는 헬퍼 서비스의 named pipe
/// (`\\.\pipe\WumaTrackerHelper`)를 통해서만 이뤄지며, 헬퍼가 실제로
/// `\\.\WumaDisplayService`를 열고 IOCTL_GET_LOCATION을 호출한다. 요청/응답 온-와이어
/// 레이아웃은 `driver/shared/ioctls.rs`가 원본(source of truth)이다.
use std::io::{Read, Write};
use std::mem;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Result};
use winapi::{
    shared::minwindef::DWORD,
    um::{
        fileapi::{CreateFileW, OPEN_EXISTING},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        minwinbase::STILL_ACTIVE,
        processthreadsapi::{GetExitCodeProcess, OpenProcess},
        winnt::{GENERIC_READ, GENERIC_WRITE, HANDLE, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

use crate::offsets::{GWorldScanConfig, WuwaOffset};
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;
use crate::types::NativeError::ValueReadError;

const PIPE_NAME: &str = r"\\.\pipe\WumaTrackerHelper";
const REQ_GET_LOCATION: u32 = 1;

#[repr(C)]
struct GetLocationResp {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
    /// 0=성공, 0xFF=미설정, 그 외=드라이버 체인 실패 단계 (진단용)
    stage: u8,
    _pad: [u8; 3],
}

pub struct WinProcDriver {
    /// 생존 확인 전용 핸들 (VM_READ 권한 없음, 헬퍼/드라이버와 무관)
    proc_handle: HANDLE,
}

impl WinProcDriver {
    /// WinProc::new와 동일한 시그니처 — native_collector.rs 무수정 교체용.
    /// scan_config는 더 이상 사용되지 않는다: GWorld 앵커/포인터 체인은 드라이버에
    /// 하드코딩되어 있고, 이 앱은 런타임에 그것을 바꿀 방법이 없다.
    pub fn new(name: &str, _cache_dir: PathBuf, _scan_config: Option<GWorldScanConfig>) -> Result<Self> {
        let pid = find_pid_by_name(name)
            .ok_or_else(|| anyhow::anyhow!("게임이 실행 중이 아닙니다."))?;

        let proc_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if proc_handle.is_null() {
            bail!("생존 확인용 프로세스 핸들을 얻지 못했습니다: {}", std::io::Error::last_os_error());
        }

        // 헬퍼가 실제로 응답 가능한지 최초 1회 점검한다 (없어도 인스턴스는 만들지만,
        // 최초 조회에서 바로 HelperUnavailable을 사용자에게 보여줄 수 있게 한다).
        if let Err(e) = call_helper_get_location() {
            log::warn!("[WinProcDriver] 헬퍼 사전 점검 실패: {}", e);
        }

        Ok(Self { proc_handle })
    }
}

impl ProcessBackend for WinProcDriver {
    fn is_alive(&self) -> bool {
        let mut exit_code: DWORD = 0;
        unsafe {
            GetExitCodeProcess(self.proc_handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as DWORD
        }
    }

    /// 헬퍼는 좌표 조회 한 가지 연산만 노출하므로 범용 메모리 읽기는 지원하지 않는다.
    fn read_bytes(&self, address: u64, _buffer: &mut [u8]) -> Result<(), NativeError> {
        Err(ValueReadError {
            message: format!(
                "WinProcDriver는 범용 메모리 읽기를 지원하지 않습니다 (addr={:X}). get_player_info(fast path)만 사용하세요.",
                address
            ),
        })
    }

    fn read_gworld(&self, _offset: &WuwaOffset) -> Result<u64, NativeError> {
        Err(ValueReadError {
            message: "WinProcDriver는 GWorld를 직접 노출하지 않습니다. get_player_info(fast path)만 사용하세요.".to_string(),
        })
    }

    fn active_offset_name(&self, _offset: &WuwaOffset) -> String {
        "helper:hardcoded-chain".to_string()
    }

    /// fast path: 좌표 조회 전체를 헬퍼 IPC 1회 요청으로 처리한다.
    fn get_player_info(&self, _offset: &WuwaOffset) -> Option<Result<crate::types::PlayerInfo, NativeError>> {
        Some(call_helper_get_location())
    }
}

impl Drop for WinProcDriver {
    fn drop(&mut self) {
        if !self.proc_handle.is_null() && self.proc_handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.proc_handle); }
        }
    }
}

unsafe impl Send for WinProcDriver {}

// ── 헬퍼 IPC ─────────────────────────────────────────────────────────────────

fn call_helper_get_location() -> Result<crate::types::PlayerInfo, NativeError> {
    let pipe = open_helper_pipe()?;
    let mut file = unsafe { <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(pipe as _) };

    if file.write_all(&REQ_GET_LOCATION.to_le_bytes()).is_err() {
        return Err(NativeError::HelperUnavailable);
    }

    let mut buf = [0u8; mem::size_of::<GetLocationResp>()];
    if file.read_exact(&mut buf).is_err() {
        return Err(NativeError::HelperUnavailable);
    }
    // file은 raw handle을 소유(drop 시 자동 CloseHandle)하므로 별도 정리가 필요 없다.

    let resp = unsafe { &*(buf.as_ptr() as *const GetLocationResp) };

    if resp.stage == 0xFF {
        return Err(NativeError::DriverUnavailable);
    }
    if resp.stage == 0xFE {
        return Err(NativeError::TargetProcessMissing);
    }
    if resp.stage != 0 {
        return Err(NativeError::PointerChainError {
            message: format!("드라이버 체인 실패 stage={}", resp.stage),
        });
    }

    Ok(crate::types::PlayerInfo {
        x: resp.x,
        y: resp.y,
        z: resp.z,
        pitch: resp.pitch,
        yaw: resp.yaw,
        roll: resp.roll,
    })
}

/// 헬퍼의 named pipe를 연다. 헬퍼 프로세스가 없거나(ENOENT류), 접근이 거부된 경우
/// (헬퍼가 이 앱 계정 외의 접근을 차단) 각각 다른 오류로 구분해 보고한다.
fn open_helper_pipe() -> Result<HANDLE, NativeError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let path_w: Vec<u16> = OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 헬퍼가 막 재시작 중이거나 이전 클라이언트를 처리 중일 수 있으므로 named pipe의
    // 표준 대기 방식(WaitNamedPipeW)으로 짧게 재시도한다.
    unsafe {
        winapi::um::namedpipeapi::WaitNamedPipeW(path_w.as_ptr(), 200);
    }

    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(5) => Err(NativeError::HelperUnauthorized), // ERROR_ACCESS_DENIED
            _ => Err(NativeError::HelperUnavailable),
        };
    }

    Ok(handle)
}

/// 헬퍼 pre-check 재시도 간격 (사전 점검 실패는 치명적이지 않으므로 짧게만 대기)
#[allow(dead_code)]
const HELPER_PRECHECK_TIMEOUT: Duration = Duration::from_millis(200);

/// 프로세스 이름으로 PID 찾기 (WinProc의 find_pid_by_name과 동일)
fn find_pid_by_name(name: &str) -> Option<u32> {
    use std::ffi::CStr;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut pe: PROCESSENTRY32 = mem::zeroed();
        pe.dwSize = mem::size_of::<PROCESSENTRY32>() as u32;

        if Process32First(snap, &mut pe) != 0 {
            loop {
                let exe = CStr::from_ptr(pe.szExeFile.as_ptr()).to_string_lossy();
                if exe == name {
                    CloseHandle(snap);
                    return Some(pe.th32ProcessID);
                }
                if Process32Next(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        None
    }
}
