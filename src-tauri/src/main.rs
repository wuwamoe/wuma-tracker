// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn main() {
    if !is_elevated() {
        // 관리자 권한이 아닐 경우, 관리자 권한으로 재실행 시도
        let exe_path = std::env::current_exe().expect("Failed to get current executable path");
        let exe_path_wide = exe_path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();

        let runas = "runas\0".encode_utf16().collect::<Vec<u16>>();

        unsafe {
            ShellExecuteW(
                0,
                runas.as_ptr(),
                exe_path_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL as i32,
            );
        }
        return;
    }

    wuma_tracker_lib::run()
}

fn is_elevated() -> bool {
    let mut is_elevated = false;
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0 {
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
            if GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                size,
                &mut size,
            ) != 0
            {
                is_elevated = elevation.TokenIsElevated != 0;
            }
            CloseHandle(token);
        }
    }
    is_elevated
}
