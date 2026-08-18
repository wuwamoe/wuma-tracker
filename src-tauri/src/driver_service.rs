//! Starts/stops the WumaDisplayService kernel driver via the Service Control
//! Manager, tied to this app's own lifecycle (see `lib.rs`'s setup and
//! post-`run_return` cleanup) instead of relying on the driver staying
//! started indefinitely after the installer's one-time `sc start`.
//!
//! The service is registered `start= demand` (see
//! `driver/scripts/install-driver.ps1` / `src-tauri/windows/wix7/main.wxs`),
//! not `start= auto` — nothing else re-starts it after a reboot, so without
//! this the driver would simply be unavailable (registered but not running)
//! until something calls `sc start` again. This keeps the driver loaded only
//! while the app that uses it is actually running, matching this project's
//! "only active when actually needed" design
//! (see `driver/docs/DIRECT-ACCESS-PLAN.md`) rather than switching the
//! service to auto-start and leaving it loaded whether or not the app ever
//! runs on a given boot.
//!
//! Both operations are best-effort: a failure here is logged, not propagated
//! as a hard error, because the coordinate-read path
//! (`win_proc_driver.rs::call_driver_get_location`) already reports
//! `DriverUnavailable` on its own if the device still can't be opened
//! afterward — there is no separate user-facing state this module needs to
//! own.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;

use winapi::um::winsvc::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_START, SERVICE_STATUS, SERVICE_STOP,
    SC_MANAGER_CONNECT, SERVICE_RUNNING, StartServiceW,
};

const SERVICE_NAME: &str = "WumaDisplayService";
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Starts the driver service if it isn't already running.
pub fn ensure_running() {
    unsafe {
        let scm = OpenSCManagerW(null_mut(), null_mut(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            log::warn!(
                "드라이버 서비스 관리자 연결 실패: {}",
                std::io::Error::last_os_error()
            );
            return;
        }

        let name_w = wide(SERVICE_NAME);
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_START | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            log::warn!(
                "드라이버 서비스를 열지 못했습니다 (설치 확인 필요): {}",
                std::io::Error::last_os_error()
            );
            CloseServiceHandle(scm);
            return;
        }

        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if QueryServiceStatus(svc, &mut status) != 0 && status.dwCurrentState == SERVICE_RUNNING {
            CloseServiceHandle(svc);
            CloseServiceHandle(scm);
            return;
        }

        if StartServiceW(svc, 0, null_mut()) == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING) {
                log::warn!("드라이버 서비스 시작 실패: {}", err);
            }
        } else {
            log::info!("드라이버 서비스 시작됨");
        }

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }
}

/// Stops the driver service.
pub fn stop() {
    unsafe {
        let scm = OpenSCManagerW(null_mut(), null_mut(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return;
        }

        let name_w = wide(SERVICE_NAME);
        let svc = OpenServiceW(scm, name_w.as_ptr(), SERVICE_STOP | SERVICE_QUERY_STATUS);
        if svc.is_null() {
            CloseServiceHandle(scm);
            return;
        }

        let mut status: SERVICE_STATUS = std::mem::zeroed();
        if ControlService(svc, SERVICE_CONTROL_STOP, &mut status) == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE) {
                log::warn!("드라이버 서비스 정지 실패: {}", err);
            }
        } else {
            log::info!("드라이버 서비스 정지됨");
        }

        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
    }
}
