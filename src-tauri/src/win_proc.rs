use crate::types::NativeError;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::{
    offsets::WuwaOffset,
    types::{FIntVector, PlayerInfo},
};
use anyhow::{Context, Result, bail};
use std::{ffi::CStr, mem};
use windows_sys::Win32::Foundation::STILL_ACTIVE;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::ProcessStatus::{EnumProcessModulesEx, LIST_MODULES_DEFAULT};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next,
            TH32CS_SNAPPROCESS,
        },
        Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
    },
};

pub struct WinProc {
    pid: u32,
    pub base_addr: u64,
    handle: HANDLE,
}

impl WinProc {
    const OFFSET: WuwaOffset = WuwaOffset {
        global_gworld: 0x85D90B0,
        uworld_persistentlevel: 0x38,
        uworld_owninggameinstance: 0x1C0,
        ulevel_lastworldorigin: 0xC8,
        ugameinstance_localplayers: 0x40,
        uplayer_playercontroller: 0x38,
        aplayercontroller_acknowlegedpawn: 0x340,
        aactor_rootcomponent: 0x1A0,
        uscenecomponent_relativelocation: 0x13C,
    };

    pub fn new(name: &str) -> Result<Self> {
        unsafe {
            let pid = Self::find_pid_by_name(name)
                .with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;

            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid);
            if handle == 0 {
                bail!(
                    "게임에 연결하지 못했습니다. OS Error: {}",
                    std::io::Error::last_os_error()
                );
            }

            let mut h_mod = 0;
            let mut cb_needed = 0;
            if EnumProcessModulesEx(
                handle,
                &mut h_mod,
                mem::size_of::<isize>() as u32,
                &mut cb_needed,
                LIST_MODULES_DEFAULT,
            ) == 0
            {
                CloseHandle(handle);
                bail!(
                    "게임 Base 주소를 가져오지 못했습니다. OS Error: {}",
                    std::io::Error::last_os_error()
                );
            }

            log::info!(
                "Process '{}' connected! PID: {}, Base Address: {:X}",
                name,
                pid,
                h_mod
            );

            Ok(WinProc {
                pid,
                base_addr: h_mod as u64,
                handle,
            })
        }
    }

    pub fn is_alive(&self) -> bool {
        if self.handle == 0 {
            return false;
        }
        unsafe {
            let mut exit_code: u32 = 0;
            if GetExitCodeProcess(self.handle, &mut exit_code) != 0 {
                return exit_code == STILL_ACTIVE as u32;
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

    fn read_memory<T: Copy>(&self, address: u64) -> Option<T> {
        if address == 0 {
            return None;
        }
        unsafe {
            let mut buffer: T = mem::zeroed();
            let mut bytes_read = 0;

            let success = ReadProcessMemory(
                self.handle,
                address as *const _,
                &mut buffer as *mut T as *mut _,
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

    unsafe fn find_pid_by_name(name: &str) -> Option<u32> {
        unsafe {
            let h_process_snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if h_process_snap == -1 {
                return None;
            }

            let mut pe32: PROCESSENTRY32 = mem::zeroed();
            pe32.dwSize = mem::size_of::<PROCESSENTRY32>() as u32;

            if Process32First(h_process_snap, &mut pe32) != 0 {
                loop {
                    let exe_file_ptr = pe32.szExeFile.as_ptr() as *const i8;
                    let exe_file = CStr::from_ptr(exe_file_ptr);
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
}

impl Drop for WinProc {
    fn drop(&mut self) {
        if self.handle != 0 {
            log::info!("Closing handle for PID {}", self.pid);
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}
