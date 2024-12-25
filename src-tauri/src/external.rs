use std::{
    ffi::{CStr, CString},
    mem,
    ptr::null_mut,
};

use winapi::{
    ctypes::c_void,
    shared::minwindef::{DWORD, HMODULE},
    um::{
        handleapi::CloseHandle,
        memoryapi::ReadProcessMemory,
        processthreadsapi::OpenProcess,
        psapi::{EnumProcessModulesEx, LIST_MODULES_DEFAULT},
        tlhelp32::{
            CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32,
            TH32CS_SNAPPROCESS,
        },
        winnt::{HANDLE, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
    },
};

use crate::{offsets::WuwaOffset, types::PlayerInfo};

pub struct WinProc {
    pid: u32,
    pub base_addr: u64,
    handle: HANDLE,
    is_init: bool,
}

impl WinProc {
    const OFFSET: WuwaOffset = WuwaOffset {
        global_gworld: 0x8506520,
        uworld_owninggameinstance: 0x1B0,
        ugameinstance_localplayers: 0x40,
        uplayer_playercontroller: 0x38,
        aplayercontroller_acknowlegedpawn: 0x340,
        aactor_rootcomponent: 0x1A0,
        uscenecomponent_relativelocation: 0x13C,
        uscenecomponent_relativerotation: 0x148,
    };

    pub fn get_location(&self) -> Result<PlayerInfo, String> {
        if !self.is_init {
            return Err(String::from("Process not initialized"));
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
            // ("RelativeLocation", offset.uscenecomponent_relativelocation),
            // ("RelativeRotation", offset.uscenecomponent_relativerotation)
        ];

        let mut last_addr = self.base_addr;
        for t in targets {
            let target = last_addr + t.1;
            let Some(ret) = self.read_memory::<u64>(target) else {
                let msg = format!("Pointer value retrieval failure({}): {:X}", t.0, target);
                // log::error!("{}", msg);
                return Err(msg);
            };
            last_addr = ret;
        }

        let target = last_addr + Self::OFFSET.uscenecomponent_relativelocation;
        let Some(location) = self.read_memory::<PlayerInfo>(target) else {
            let msg = format!(
                "Value retreival failure({}): {:X}",
                "RelativeLocation, RelativeRotation", target
            );
            log::error!("{}", msg);
            return Err(msg);
        };

        Ok(location)
    }

    pub fn init(&mut self) -> bool {
        unsafe {
            self.handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, self.pid);
            let mut is_init = !self.handle.is_null();
            if is_init {
                let mut h_mod: HMODULE = null_mut();
                let mut cb_needed = 0;
                if EnumProcessModulesEx(
                    self.handle,
                    &mut h_mod,
                    std::mem::size_of::<HMODULE>() as DWORD,
                    &mut cb_needed,
                    LIST_MODULES_DEFAULT,
                ) == 0
                {
                    is_init = false;
                }
                self.base_addr = h_mod as u64;
                log::info!("connected! base address is {:X}", self.base_addr);
            }
            self.is_init = is_init;
            return is_init;
        }
    }

    pub fn read_memory<T: Copy>(&self, address: u64) -> Option<T> {
        if !self.is_init {
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

            // Check if read was successful and the correct amount of bytes was read
            if success != 0 && bytes_read == mem::size_of::<T>() {
                Some(buffer)
            } else {
                None
            }
        }
    }

    pub fn from_name(name: &str) -> Result<WinProc, &str> {
        unsafe {
            let process_name = CString::new(name).expect("Error creating CString of process_name");
            let proc_name_bytes = process_name.as_bytes_with_nul();
            let mut proc_running = false;

            let h_process_snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if h_process_snap.is_null() {
                return Err("Process snap unavailable");
            }

            let mut pe32: PROCESSENTRY32 = std::mem::zeroed();
            pe32.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

            if Process32First(h_process_snap, &mut pe32) != 0 {
                loop {
                    let exe_file = CStr::from_ptr(pe32.szExeFile.as_ptr());
                    if exe_file.to_bytes_with_nul() == proc_name_bytes {
                        proc_running = true;
                        break;
                    }

                    if Process32Next(h_process_snap, &mut pe32) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(h_process_snap);

            return if proc_running {
                Ok(WinProc {
                    pid: pe32.th32ProcessID,
                    base_addr: 0,
                    handle: std::ptr::null_mut(),
                    is_init: false,
                })
            } else {
                Err("Process with given name not found")
            };
        }
    }
}

impl Drop for WinProc {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

unsafe impl Send for WinProc {}
