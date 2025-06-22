use std::{ffi::CStr, mem, ptr::null_mut};

use crate::types::NativeError;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::{
    offsets::WuwaOffset,
    types::{FIntVector, PlayerInfo},
};
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
}

impl WinProc {
    const OFFSET: WuwaOffset = WuwaOffset {
        global_gworld: 0x84C55C0,
        uworld_persistentlevel: 0x38,
        uworld_owninggameinstance: 0x1C0,
        ulevel_lastworldorigin: 0xC8,
        ugameinstance_localplayers: 0x40,
        uplayer_playercontroller: 0x38,
        aplayercontroller_acknowlegedpawn: 0x340,
        aactor_rootcomponent: 0x1A0,
        uscenecomponent_relativelocation: 0x13C,
    };

    /// 프로세스 이름으로 WinProc 인스턴스를 생성합니다.
    /// PID 찾기, 핸들 열기, 베이스 주소 가져오기를 모두 수행합니다.
    pub fn new(name: &str) -> Result<Self> {
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
            })
        }
    }

    /// 프로세스가 여전히 실행 중인지 확인합니다.
    pub fn is_alive(&self) -> bool {
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

    pub fn get_location(&self) -> Result<PlayerInfo, NativeError> {
        if !self.is_alive() {
            return Err(NativeError::ProcessTerminated);
        }

        let targets = [
            ("GWorld", Self::OFFSET.global_gworld),
            ("OwningGameInstance", Self::OFFSET.uworld_owninggameinstance),
            (
                "TArray<*LocalPlayers>",
                Self::OFFSET.ugameinstance_localplayers,
            ),
            ("LocalPlayer", 0),
            ("PlayerController", Self::OFFSET.uplayer_playercontroller),
            ("APawn", Self::OFFSET.aplayercontroller_acknowlegedpawn),
            ("RootComponent", Self::OFFSET.aactor_rootcomponent),
        ];

        let mut last_addr = self.base_addr;
        for t in targets {
            let target = last_addr + t.1;
            last_addr = self
                .read_memory::<u64>(target)
                .ok_or_else(|| PointerChainError {
                    message: format!("'{}' 위치 ({:X})의 주소 값을 읽지 못했습니다.", t.0, target),
                })?;
        }

        let target = last_addr + Self::OFFSET.uscenecomponent_relativelocation;
        let location = self
            .read_memory::<PlayerInfo>(target)
            .ok_or_else(|| ValueReadError {
                message: format!(
                    "RelativeLocation 위치 ({:X})의 값을 읽지 못했습니다.",
                    target
                ),
            })?;

        let target_worldorigin = [
            ("GWorld", Self::OFFSET.global_gworld),
            ("PersistentLevel", Self::OFFSET.uworld_persistentlevel),
        ];

        last_addr = self.base_addr;
        for t in target_worldorigin {
            let target = last_addr + t.1;
            last_addr = self
                .read_memory::<u64>(target)
                .ok_or_else(|| PointerChainError {
                    message: format!(
                        "WorldOrigin을 위한 '{}' 위치 ({:X})의 주소 값을 읽지 못했습니다.",
                        t.0, target
                    ),
                })?;
        }

        let target = last_addr + Self::OFFSET.ulevel_lastworldorigin;
        let root_location =
            self.read_memory::<FIntVector>(target)
                .ok_or_else(|| ValueReadError {
                    message: format!(
                        "LastWorldOrigin 위치 ({:X})의 값을 읽지 못했습니다.",
                        target
                    ),
                })?;

        Ok(PlayerInfo {
            x: location.x + (root_location.x as f32),
            y: location.y + (root_location.y as f32),
            z: location.z + (root_location.z as f32),
            pitch: location.pitch,
            yaw: location.yaw,
            roll: location.roll,
        })
    }

    // 이 메서드는 이제 private으로 만들어 외부에서 직접 호출하지 않도록 할 수 있습니다.
    fn read_memory<T: Copy>(&self, address: u64) -> Option<T> {
        // 주소 0은 유효하지 않으므로 미리 차단합니다. 포인터 체인이 끊겼을 때 흔히 발생합니다.
        if address == 0 {
            return None;
        }
        unsafe {
            let mut buffer: T = mem::zeroed();
            let mut bytes_read = 0;

            let success = ReadProcessMemory(
                self.handle,
                address as *const c_void,
                &mut buffer as *mut T as *mut c_void,
                mem::size_of::<T>(),
                &mut bytes_read,
            );

            if success != 0 && bytes_read == mem::size_of::<T>() {
                Some(buffer)
            } else {
                None
            }
        }
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
