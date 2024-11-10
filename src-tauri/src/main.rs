// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::os::windows::ffi::OsStrExt;

use winapi::shared::ntdef::NULL;
use winapi::um::processthreadsapi::OpenProcessToken;
use winapi::um::securitybaseapi::GetTokenInformation;
use winapi::um::shellapi::ShellExecuteW;
use winapi::um::winnt::{TokenElevation, HANDLE, TOKEN_ELEVATION, TOKEN_QUERY};
use winapi::um::winuser::SW_SHOWNORMAL;

fn main() {
    if !is_elevated() {
        // 관리자 권한이 아닐 경우, 관리자 권한으로 재실행 시도
        let exe_path = std::env::current_exe().expect("Failed to get current executable path");
        let exe_path = exe_path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();

        unsafe {
            ShellExecuteW(
                NULL as _,
                "runas\0".encode_utf16().collect::<Vec<u16>>().as_ptr(),
                exe_path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
        return;
    }
    
    wuma_helper_lib::run()
}

fn is_elevated() -> bool {
    let mut is_elevated = false;
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(winapi::um::processthreadsapi::GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0 {
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
            if GetTokenInformation(token, TokenElevation, &mut elevation as *mut _ as *mut _, size, &mut size) != 0 {
                is_elevated = elevation.TokenIsElevated != 0;
            }
            winapi::um::handleapi::CloseHandle(token);
        }
    }
    is_elevated
}