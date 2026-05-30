use std::{ffi::CStr, mem, path::PathBuf, ptr::null_mut};

use crate::process_backend::{ProcessBackend, select_player_info};
use crate::types::NativeError;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::{offsets::WuwaOffset, types::PlayerInfo};
use anyhow::{Context, Result, bail};
use winapi::um::minwinbase::STILL_ACTIVE;
use winapi::um::processthreadsapi::GetExitCodeProcess;
use winapi::{
    ctypes::c_void,
    shared::minwindef::{DWORD, HMODULE},
    um::{
        handleapi::CloseHandle,
        memoryapi::ReadProcessMemory,
        processthreadsapi::OpenProcess,
        psapi::{EnumProcessModulesEx, LIST_MODULES_DEFAULT},
        tlhelp32::{
            CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next,
            TH32CS_SNAPPROCESS,
        },
        winnt::{HANDLE, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
    },
};
pub struct WinProc {
    pid: u32,
    pub base_addr: u64,
    handle: HANDLE,
    offset: Option<WuwaOffset>,
}

impl WinProc {
    /// 프로세스 이름으로 WinProc 인스턴스를 생성합니다.
    /// PID 찾기, 핸들 열기, 베이스 주소 가져오기를 모두 수행합니다.
    pub fn new(name: &str, _cache_dir: PathBuf) -> Result<Self> {
        unsafe {
            let pid = Self::find_pid_by_name(name)
                .with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;

            // 핸들 열기
            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid);
            if handle.is_null() {
                bail!(
                    "게임에 연결하지 못했습니다. OS Error: {}",
                    std::io::Error::last_os_error()
                );
            }

            // 베이스 주소 가져오기
            let mut h_mod: HMODULE = null_mut();
            let mut cb_needed = 0;
            if EnumProcessModulesEx(
                handle,
                &mut h_mod,
                size_of::<HMODULE>() as DWORD,
                &mut cb_needed,
                LIST_MODULES_DEFAULT,
            ) == 0
            {
                CloseHandle(handle); // 실패 시 생성된 핸들을 닫아줍니다.
                bail!(
                    "게임 Base 주소를 가져오지 못했습니다. OS Error: {}",
                    std::io::Error::last_os_error()
                );
            }

            log::info!(
                "Process '{}' connected! PID: {}, Base Address: {:X}",
                name,
                pid,
                h_mod as u64
            );

            Ok(WinProc {
                pid,
                base_addr: h_mod as u64,
                handle,
                offset: None,
            })
        }
    }

    pub async fn get_location(
        &mut self,
        available_offsets: &Option<Vec<WuwaOffset>>,
    ) -> Result<PlayerInfo, NativeError> {
        let Some(variants) = available_offsets else {
            return Err(PointerChainError {
                message: "오프셋 데이터를 불러오는 중입니다...".to_string(),
            });
        };

        let mut cached_offset = self.offset.take();
        let result = select_player_info(self, &mut cached_offset, variants);
        self.offset = cached_offset;
        result
    }

    pub fn get_active_offset_name(&self) -> Option<String> {
        self.offset.as_ref().map(|o| o.name.clone())
    }

    // 헬퍼 함수로 분리하여 new에서 사용
    unsafe fn find_pid_by_name(name: &str) -> Option<u32> {
        let h_process_snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if h_process_snap.is_null() {
            return None;
        }

        let mut pe32: PROCESSENTRY32 = mem::zeroed();
        pe32.dwSize = size_of::<PROCESSENTRY32>() as u32;

        if Process32First(h_process_snap, &mut pe32) != 0 {
            loop {
                let exe_file = CStr::from_ptr(pe32.szExeFile.as_ptr());
                if exe_file.to_string_lossy() == name {
                    CloseHandle(h_process_snap);
                    return Some(pe32.th32ProcessID);
                }
                if Process32Next(h_process_snap, &mut pe32) == 0 {
                    break;
                }
            }
        }

        CloseHandle(h_process_snap);
        None
    }
}

impl ProcessBackend for WinProc {
    fn is_alive(&self) -> bool {
        if self.handle.is_null() {
            return false;
        }
        unsafe {
            let mut exit_code: DWORD = 0;
            if GetExitCodeProcess(self.handle, &mut exit_code) != 0 {
                return exit_code == STILL_ACTIVE;
            }
            false
        }
    }

    fn read_bytes(&self, address: u64, buffer: &mut [u8]) -> Result<(), NativeError> {
        unsafe {
            let mut bytes_read = 0;

            let success = ReadProcessMemory(
                self.handle,
                address as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len(),
                &mut bytes_read,
            );

            if success != 0 && bytes_read == buffer.len() {
                Ok(())
            } else {
                Err(ValueReadError {
                    message: format!(
                        "ReadProcessMemory 실패: address={:X}, bytes_read={}/{}",
                        address,
                        bytes_read,
                        buffer.len()
                    ),
                })
            }
        }
    }

    fn read_gworld(&self, offset: &WuwaOffset) -> Result<u64, NativeError> {
        let target = self.base_addr + offset.global_gworld;
        self.read_memory::<u64>(target)
            .map_err(|e| PointerChainError {
                message: format!(
                    "'GWorld' 위치 ({:X})의 주소 값을 읽지 못했습니다: {}",
                    target, e
                ),
            })
    }
}

impl Drop for WinProc {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            log::info!("Closing handle for PID {}", self.pid);
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

unsafe impl Send for WinProc {}
