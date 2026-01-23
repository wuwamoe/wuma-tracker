// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use winapi::um::processthreadsapi::OpenProcessToken;
use winapi::um::securitybaseapi::GetTokenInformation;
use winapi::um::winnt::{HANDLE, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};

fn main() {
    wuma_tracker_lib::run()
}

fn is_elevated() -> bool {
    let mut is_elevated = false;
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            winapi::um::processthreadsapi::GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ) != 0
        {
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
            winapi::um::handleapi::CloseHandle(token);
        }
    }
    is_elevated
}
