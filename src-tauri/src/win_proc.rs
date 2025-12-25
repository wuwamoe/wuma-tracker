use std::{ffi::CStr, mem, ptr::null_mut};
use std::f32::consts::PI;
use crate::types::{FTransformDouble, NativeError};
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
const OFFSET_VARIANTS: [WuwaOffset; 2] = [
    WuwaOffset {
        name: "v2.8.0",
        global_gworld: 0x8E752E8,
        uworld_persistentlevel: 0x38,
        uworld_owninggameinstance: 0x1B8,
        ulevel_lastworldorigin: 0xC8,
        ugameinstance_localplayers: 0x40,
        uplayer_playercontroller: 0x38,
        aplayercontroller_acknowlegedpawn: 0x340,
        aactor_rootcomponent: 0x1A0,
        uscenecomponent_componenttoworld: 0x1E0,
    },
    WuwaOffset {
        name: "v3.0.0",
        global_gworld: 0x8CBB6C0,
        uworld_persistentlevel: 0x38,
        uworld_owninggameinstance: 0x1B8,
        ulevel_lastworldorigin: 0xC8,
        ugameinstance_localplayers: 0x40,
        uplayer_playercontroller: 0x38,
        aplayercontroller_acknowlegedpawn: 0x340,
        aactor_rootcomponent: 0x1A0,
        uscenecomponent_componenttoworld: 0x1E0,
    }
];

pub struct WinProc {
    pid: u32,
    pub base_addr: u64,
    handle: HANDLE,
    offset: Option<WuwaOffset>,
}

impl WinProc {
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
                offset: None,
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

    pub fn get_location(&mut self) -> Result<PlayerInfo, NativeError> {
        if !self.is_alive() {
            return Err(NativeError::ProcessTerminated);
        }

        // 이미 성공한 오프셋이 있다면 그것을 사용합니다.
        if let Some(offset) = self.offset {
            return self.get_location_with_offset(&offset);
        }

        // 성공한 오프셋이 없다면, 모든 변형을 시도합니다.
        for (i, offset) in OFFSET_VARIANTS.iter().enumerate() {
            if let Ok(location) = self.get_location_with_offset(offset) {
                log::info!("Offset variant #{} ({}) succeeded. Caching it.", i + 1, offset.name);
                // 성공하면 오프셋을 저장하고 결과를 반환합니다.
                self.offset = Some(*offset);
                return Ok(location);
            }
        }
        
        // 모든 오프셋이 실패한 경우 에러를 반환합니다.
        Err(PointerChainError {
            message: "사용 가능한 버전 값을 찾지 못했습니다.".to_string(),
        })
    }

    pub fn get_active_offset_name(&self) -> Option<&'static str> {
        self.offset.map(|o| o.name)
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

    fn quat_to_euler(x: f32, y: f32, z: f32, w: f32) -> (f32, f32, f32) {
        // 언리얼 엔진 좌표계 변환 로직
        // Roll (X축 회전)
        let sinr_cosp = 2.0 * (w * x + y * z);
        let cosr_cosp = 1.0 - 2.0 * (x * x + y * y);
        let roll = sinr_cosp.atan2(cosr_cosp);

        // Pitch (Y축 회전)
        let sinp = 2.0 * (w * y - z * x);
        let pitch = if sinp.abs() >= 1.0 {
            (PI / 2.0).copysign(sinp) // 90도 제한
        } else {
            sinp.asin()
        };

        // Yaw (Z축 회전)
        let siny_cosp = 2.0 * (w * z + x * y);
        let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
        let yaw = siny_cosp.atan2(cosy_cosp);

        // 라디안 -> 도(Degree) 변환
        ((roll * 180.0 / PI), (pitch * 180.0 / PI), (yaw * 180.0 / PI))
    }

    fn get_location_with_offset(&self, offset: &WuwaOffset) -> Result<PlayerInfo, NativeError> {
        let targets = [
            ("GWorld", offset.global_gworld),
            ("OwningGameInstance", offset.uworld_owninggameinstance),
            ("TArray<*LocalPlayers>", offset.ugameinstance_localplayers),
            ("LocalPlayer", 0),
            ("PlayerController", offset.uplayer_playercontroller),
            ("APawn", offset.aplayercontroller_acknowlegedpawn),
            ("RootComponent", offset.aactor_rootcomponent),
        ];

        let mut last_addr = self.base_addr;
        for t in targets {
            let target = last_addr + t.1;
            last_addr = self.read_memory::<u64>(target).ok_or_else(|| PointerChainError {
                message: format!("'{}' 위치 ({:X})의 주소 값을 읽지 못했습니다.", t.0, target),
            })?;
        }

        let target = last_addr + offset.uscenecomponent_componenttoworld;
        let location = self.read_memory::<FTransformDouble>(target).ok_or_else(|| ValueReadError {
            message: format!("FTransform 위치 ({:X})의 값을 읽지 못했습니다.", target),
        })?;

        let (roll, pitch, yaw) = Self::quat_to_euler(location.rot_x, location.rot_y, location.rot_z, location.rot_w);

        let target_worldorigin = [
            ("GWorld", offset.global_gworld),
            ("PersistentLevel", offset.uworld_persistentlevel),
        ];

        last_addr = self.base_addr;
        for t in target_worldorigin {
            let target = last_addr + t.1;
            last_addr = self.read_memory::<u64>(target).ok_or_else(|| PointerChainError {
                message: format!("WorldOrigin을 위한 '{}' 위치 ({:X})의 주소 값을 읽지 못했습니다.", t.0, target),
            })?;
        }

        let target = last_addr + offset.ulevel_lastworldorigin;
        let root_location = self.read_memory::<FIntVector>(target).ok_or_else(|| ValueReadError {
            message: format!("LastWorldOrigin 위치 ({:X})의 값을 읽지 못했습니다.", target),
        })?;

        Ok(PlayerInfo {
            x: location.loc_x + (root_location.x as f32),
            y: location.loc_y + (root_location.y as f32),
            z: location.loc_z + (root_location.z as f32),
            pitch,
            yaw,
            roll,
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
