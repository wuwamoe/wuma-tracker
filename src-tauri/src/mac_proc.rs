use crate::offsets::WuwaOffset;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::types::{FIntVector, FTransformDouble, NativeError, PlayerInfo};
use anyhow::{Context, Result, bail};
use std::f32::consts::PI;
use std::ffi::CStr;
use std::mem;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::Path;
use std::process::Command;

type KernReturn = i32;
type MachPort = u32;
type MachVmAddress = u64;
type MachVmSize = u64;

const KERN_SUCCESS: KernReturn = 0;
const MACH_PORT_NULL: MachPort = 0;
const MACH_EXECUTE_BASE_VMADDR: u64 = 0x1000_0000_0;
const GWORLD_SYMBOL_NAME: &str = "_GWorld";
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

unsafe extern "C" {
    static mach_task_self_: MachPort;

    fn task_for_pid(task: MachPort, pid: c_int, target_task: *mut MachPort) -> KernReturn;
    fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    fn mach_vm_read_overwrite(
        target_task: MachPort,
        address: MachVmAddress,
        size: MachVmSize,
        data: MachVmAddress,
        out_size: *mut MachVmSize,
    ) -> KernReturn;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: c_uint) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

pub struct MacProc {
    pid: c_int,
    task: MachPort,
    gworld_symbol_addr: u64,
    offset: Option<WuwaOffset>,
}

impl MacProc {
    pub fn new(name: &str) -> Result<Self> {
        let pid = Self::find_pid_by_name(name)
            .with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;
        let task = Self::open_task(pid)?;
        let process_path = Self::process_path(pid)?;
        let load_addr = Self::load_address(pid)?;
        let gworld_file_addr = Self::symbol_address(&process_path, GWORLD_SYMBOL_NAME)?;
        let slide = load_addr.saturating_sub(MACH_EXECUTE_BASE_VMADDR);
        let gworld_symbol_addr = gworld_file_addr + slide;

        log::info!(
            "Process '{}' connected! PID: {}, Load Address: {:X}, _GWorld: {:X}",
            name,
            pid,
            load_addr,
            gworld_symbol_addr
        );

        Ok(Self {
            pid,
            task,
            gworld_symbol_addr,
            offset: None,
        })
    }

    pub fn is_alive(&self) -> bool {
        unsafe { kill(self.pid, 0) == 0 }
    }

    pub async fn get_location(
        &mut self,
        available_offsets: &Option<Vec<WuwaOffset>>,
    ) -> Result<PlayerInfo, NativeError> {
        if !self.is_alive() {
            return Err(NativeError::ProcessTerminated);
        }

        let Some(variants) = available_offsets else {
            return Err(PointerChainError {
                message: "오프셋 데이터를 불러오는 중입니다...".to_string(),
            });
        };

        if let Some(offset) = &self.offset {
            return self.get_location_with_offset(offset);
        }

        for (i, offset) in variants.iter().enumerate() {
            if let Ok(location) = self.get_location_with_offset(offset) {
                log::info!("Mac offset variant #{} ({}) succeeded.", i + 1, offset.name);
                self.offset = Some(offset.clone());
                return Ok(location);
            }
        }

        Err(PointerChainError {
            message: "사용 가능한 Mac 버전 값을 찾지 못했습니다.".to_string(),
        })
    }

    pub fn get_active_offset_name(&self) -> Option<String> {
        self.offset.as_ref().map(|o| format!("mac:{}", o.name))
    }

    fn get_location_with_offset(&self, offset: &WuwaOffset) -> Result<PlayerInfo, NativeError> {
        let gworld = self
            .read_memory::<u64>(self.gworld_symbol_addr)
            .ok_or_else(|| PointerChainError {
                message: format!(
                    "_GWorld 위치 ({:X})의 주소 값을 읽지 못했습니다.",
                    self.gworld_symbol_addr
                ),
            })?;

        let targets = [
            ("OwningGameInstance", offset.uworld_owninggameinstance),
            ("TArray<*LocalPlayers>", offset.ugameinstance_localplayers),
            ("LocalPlayer", 0),
            ("PlayerController", offset.uplayer_playercontroller),
            ("APawn", offset.aplayercontroller_acknowlegedpawn),
            ("RootComponent", offset.aactor_rootcomponent),
        ];

        let mut last_addr = gworld;
        for t in targets {
            let target = last_addr + t.1;
            last_addr = self
                .read_memory::<u64>(target)
                .ok_or_else(|| PointerChainError {
                    message: format!("'{}' 위치 ({:X})의 주소 값을 읽지 못했습니다.", t.0, target),
                })?;
        }

        let target = last_addr + offset.uscenecomponent_componenttoworld;
        let location = self
            .read_memory::<FTransformDouble>(target)
            .ok_or_else(|| ValueReadError {
                message: format!("FTransform 위치 ({:X})의 값을 읽지 못했습니다.", target),
            })?;

        let (roll, pitch, yaw) = Self::quat_to_euler(
            location.rot_x,
            location.rot_y,
            location.rot_z,
            location.rot_w,
        );

        let persistent_level_addr = gworld + offset.uworld_persistentlevel;
        let persistent_level = self
            .read_memory::<u64>(persistent_level_addr)
            .ok_or_else(|| PointerChainError {
                message: format!(
                    "WorldOrigin을 위한 PersistentLevel 위치 ({:X})의 주소 값을 읽지 못했습니다.",
                    persistent_level_addr
                ),
            })?;

        let target = persistent_level + offset.ulevel_lastworldorigin;
        let root_location =
            self.read_memory::<FIntVector>(target)
                .ok_or_else(|| ValueReadError {
                    message: format!(
                        "LastWorldOrigin 위치 ({:X})의 값을 읽지 못했습니다.",
                        target
                    ),
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

    fn read_memory<T: Copy>(&self, address: u64) -> Option<T> {
        if address == 0 {
            return None;
        }

        unsafe {
            let mut buffer: T = mem::zeroed();
            let mut bytes_read: MachVmSize = 0;
            let success = mach_vm_read_overwrite(
                self.task,
                address,
                mem::size_of::<T>() as MachVmSize,
                &mut buffer as *mut T as MachVmAddress,
                &mut bytes_read,
            );

            if success == KERN_SUCCESS && bytes_read == mem::size_of::<T>() as MachVmSize {
                Some(buffer)
            } else {
                None
            }
        }
    }

    fn find_pid_by_name(name: &str) -> Option<c_int> {
        let output = Command::new("/usr/bin/pgrep")
            .arg("-x")
            .arg(name)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<c_int>().ok())
    }

    fn open_task(pid: c_int) -> Result<MachPort> {
        let mut task = MACH_PORT_NULL;
        let kr = unsafe { task_for_pid(mach_task_self(), pid, &mut task) };
        if kr != KERN_SUCCESS || task == MACH_PORT_NULL {
            bail!(
                "게임 프로세스 권한을 얻지 못했습니다. macOS에서는 com.apple.security.cs.debugger entitlement로 앱을 서명해야 합니다. kern_return={}",
                kr
            );
        }
        Ok(task)
    }

    fn process_path(pid: c_int) -> Result<String> {
        let mut buffer = vec![0 as c_char; PROC_PIDPATHINFO_MAXSIZE];
        let len = unsafe {
            proc_pidpath(
                pid,
                buffer.as_mut_ptr() as *mut c_void,
                PROC_PIDPATHINFO_MAXSIZE as c_uint,
            )
        };

        if len <= 0 {
            bail!("게임 실행 파일 경로를 확인하지 못했습니다.");
        }

        let path = unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(path)
    }

    fn load_address(pid: c_int) -> Result<u64> {
        let output = Command::new("/usr/bin/vmmap")
            .arg("-summary")
            .arg(pid.to_string())
            .output()
            .context("vmmap 실행 실패")?;

        if !output.status.success() {
            bail!(
                "게임 Load Address를 확인하지 못했습니다: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let Some(value) = line.trim().strip_prefix("Load Address:") else {
                continue;
            };
            let value = value.trim().trim_start_matches("0x");
            return u64::from_str_radix(value, 16).context("Load Address 파싱 실패");
        }

        bail!("vmmap 출력에서 Load Address를 찾지 못했습니다.");
    }

    fn symbol_address(path: impl AsRef<Path>, symbol: &str) -> Result<u64> {
        let output = Command::new("/usr/bin/nm")
            .arg(path.as_ref())
            .output()
            .context("nm 실행 실패")?;

        if !output.status.success() {
            bail!(
                "게임 심볼 테이블을 확인하지 못했습니다: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let mut parts = line.split_whitespace();
            let Some(address) = parts.next() else {
                continue;
            };
            let _kind = parts.next();
            let Some(name) = parts.next() else {
                continue;
            };
            if name == symbol {
                return u64::from_str_radix(address, 16)
                    .with_context(|| format!("{} 주소 파싱 실패", symbol));
            }
        }

        bail!("{} 심볼을 찾지 못했습니다.", symbol);
    }

    fn quat_to_euler(x: f32, y: f32, z: f32, w: f32) -> (f32, f32, f32) {
        let sinr_cosp = 2.0 * (w * x + y * z);
        let cosr_cosp = 1.0 - 2.0 * (x * x + y * y);
        let roll = sinr_cosp.atan2(cosr_cosp);

        let sinp = 2.0 * (w * y - z * x);
        let pitch = if sinp.abs() >= 1.0 {
            (PI / 2.0).copysign(sinp)
        } else {
            sinp.asin()
        };

        let siny_cosp = 2.0 * (w * z + x * y);
        let cosy_cosp = 1.0 - 2.0 * (y * y + z * z);
        let yaw = siny_cosp.atan2(cosy_cosp);

        (roll * 180.0 / PI, pitch * 180.0 / PI, yaw * 180.0 / PI)
    }
}

impl Drop for MacProc {
    fn drop(&mut self) {
        if self.task != MACH_PORT_NULL {
            log::info!("Closing Mach task port for PID {}", self.pid);
            unsafe {
                mach_port_deallocate(mach_task_self(), self.task);
            }
        }
    }
}

fn mach_task_self() -> MachPort {
    unsafe { mach_task_self_ }
}
