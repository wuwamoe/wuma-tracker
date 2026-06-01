use crate::offsets::WuwaOffset;
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;
use crate::types::NativeError::PointerChainError;
use anyhow::{Context, Result, bail};
use goblin::mach::constants::cputype;
use goblin::mach::load_command::CommandVariant;
use goblin::mach::{Mach, MachO, SingleArch};
use serde::{Deserialize, Serialize};
use std::ffi::CStr;
use std::fs;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

type KernReturn = i32;
type MachPort = u32;
type MachVmAddress = u64;
type MachVmSize = u64;
type Natural = u32;
type MachMsgTypeNumber = u32;

const KERN_SUCCESS: KernReturn = 0;
const MACH_PORT_NULL: MachPort = 0;
const MACH_EXECUTE_BASE_VMADDR: u64 = 0x1000_0000_0;
const GWORLD_SYMBOL_NAME: &str = "_GWorld";
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;
const CACHE_FILE: &str = "mac_gworld_symbol_cache.json";
const TASK_DYLD_INFO: c_int = 17;
const TASK_DYLD_INFO_COUNT: MachMsgTypeNumber =
    (std::mem::size_of::<TaskDyldInfo>() / std::mem::size_of::<Natural>()) as MachMsgTypeNumber;
const MAX_REMOTE_PATH_LEN: usize = 4096;

unsafe extern "C" {
    static mach_task_self_: MachPort;

    fn task_for_pid(task: MachPort, pid: c_int, target_task: *mut MachPort) -> KernReturn;
    fn task_info(
        target_task: MachPort,
        flavor: c_int,
        task_info_out: *mut Natural,
        task_info_count: *mut MachMsgTypeNumber,
    ) -> KernReturn;
    fn mach_port_deallocate(task: MachPort, name: MachPort) -> KernReturn;
    fn mach_vm_read_overwrite(
        target_task: MachPort,
        address: MachVmAddress,
        size: MachVmSize,
        data: MachVmAddress,
        out_size: *mut MachVmSize,
    ) -> KernReturn;
    fn proc_listallpids(buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: c_uint) -> c_int;
    fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: c_uint) -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

pub struct MacProc {
    pid: c_int,
    task: MachTaskPort,
    gworld_symbol_addr: u64,
}

struct MachTaskPort {
    port: MachPort,
}

#[derive(Default, Serialize, Deserialize)]
struct GworldSymbolCache {
    entries: Vec<GworldSymbolCacheEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct GworldSymbolCacheEntry {
    path: String,
    len: u64,
    modified_secs: u64,
    #[serde(default)]
    uuid: Option<String>,
    symbol: String,
    address: u64,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct TaskDyldInfo {
    all_image_info_addr: u64,
    all_image_info_size: u64,
    all_image_info_format: c_int,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct DyldAllImageInfosPrefix {
    version: u32,
    info_array_count: u32,
    info_array: u64,
}

#[repr(C)]
#[derive(Copy, Clone, Default)]
struct DyldImageInfo {
    image_load_address: u64,
    image_file_path: u64,
    image_file_mod_date: u64,
}

impl MacProc {
    pub fn new(name: &str, cache_dir: PathBuf) -> Result<Self> {
        let pid = Self::find_pid_by_name(name)
            .with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;
        let task = MachTaskPort::open(pid)?;
        let process_path = Self::process_path(pid)?;
        let load_addr = Self::load_address(&task, &process_path)?;
        let gworld_file_addr =
            Self::symbol_address_with_cache(&process_path, GWORLD_SYMBOL_NAME, &cache_dir)?;
        let slide = load_addr
            .checked_sub(MACH_EXECUTE_BASE_VMADDR)
            .with_context(|| format!("잘못된 Mach-O Load Address: {:X}", load_addr))?;
        let gworld_symbol_addr = gworld_file_addr
            .checked_add(slide)
            .context("_GWorld 런타임 주소 계산 overflow")?;

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
        })
    }

    fn find_pid_by_name(name: &str) -> Option<c_int> {
        let mut capacity = 2048usize;
        loop {
            let mut pids = vec![0 as c_int; capacity];
            let count = unsafe {
                proc_listallpids(
                    pids.as_mut_ptr() as *mut c_void,
                    (pids.len() * std::mem::size_of::<c_int>()) as c_int,
                )
            };

            if count <= 0 {
                return None;
            }

            let count = count as usize;
            if count >= capacity {
                capacity *= 2;
                continue;
            }

            return pids.into_iter().take(count).find(|pid| {
                if *pid <= 0 {
                    return false;
                }
                Self::process_name(*pid)
                    .as_deref()
                    .is_some_and(|process_name| process_name == name)
            });
        }
    }

    fn process_name(pid: c_int) -> Option<String> {
        let mut buffer = vec![0 as c_char; PROC_PIDPATHINFO_MAXSIZE];
        let len = unsafe {
            proc_name(
                pid,
                buffer.as_mut_ptr() as *mut c_void,
                PROC_PIDPATHINFO_MAXSIZE as c_uint,
            )
        };

        if len <= 0 {
            return None;
        }

        Some(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
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

    fn load_address(task: &MachTaskPort, process_path: &str) -> Result<u64> {
        let dyld_info = task.dyld_info()?;
        let infos = task.read_memory::<DyldAllImageInfosPrefix>(dyld_info.all_image_info_addr)?;
        if infos.info_array_count == 0 || infos.info_array == 0 {
            bail!("dyld image list가 비어 있습니다.");
        }

        let process_file_name = Path::new(process_path)
            .file_name()
            .and_then(|name| name.to_str());
        let mut file_name_match = None;

        for i in 0..infos.info_array_count {
            let address =
                infos.info_array + (i as u64 * std::mem::size_of::<DyldImageInfo>() as u64);
            let image = task.read_memory::<DyldImageInfo>(address)?;
            if image.image_load_address == 0 || image.image_file_path == 0 {
                continue;
            }

            let path = task.read_remote_c_string(image.image_file_path, MAX_REMOTE_PATH_LEN)?;
            if path == process_path {
                return Ok(image.image_load_address);
            }

            if file_name_match.is_none()
                && process_file_name.is_some_and(|name| {
                    Path::new(&path)
                        .file_name()
                        .and_then(|image_name| image_name.to_str())
                        .is_some_and(|image_name| image_name == name)
                })
            {
                file_name_match = Some(image.image_load_address);
            }
        }

        file_name_match.with_context(|| {
            "dyld image list에서 main executable load address를 찾지 못했습니다.".to_string()
        })
    }

    fn symbol_address_with_cache(
        path: impl AsRef<Path>,
        symbol: &str,
        cache_dir: &Path,
    ) -> Result<u64> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).context("게임 실행 파일 metadata 확인 실패")?;
        let path_string = path.to_string_lossy().into_owned();
        let uuid = Self::macho_uuid(path).unwrap_or_else(|e| {
            log::warn!("Failed to read Mach-O UUID for cache key: {}", e);
            None
        });
        let modified_secs = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let cache_path = cache_dir.join(CACHE_FILE);
        let mut cache = Self::load_symbol_cache(&cache_path);

        if let Some(entry) = cache.entries.iter().find(|entry| {
            entry.path == path_string
                && entry.len == metadata.len()
                && entry.modified_secs == modified_secs
                && entry.uuid == uuid
                && entry.symbol == symbol
        }) {
            log::info!("Using cached Mach-O symbol {}: {:X}", symbol, entry.address);
            return Ok(entry.address);
        }

        let address = Self::symbol_address(path, symbol)?;
        cache
            .entries
            .retain(|entry| !(entry.path == path_string && entry.symbol == symbol));
        cache.entries.push(GworldSymbolCacheEntry {
            path: path_string,
            len: metadata.len(),
            modified_secs,
            uuid,
            symbol: symbol.to_string(),
            address,
        });
        Self::save_symbol_cache(&cache_path, &cache);
        Ok(address)
    }

    fn load_symbol_cache(path: &Path) -> GworldSymbolCache {
        let Ok(data) = fs::read_to_string(path) else {
            return GworldSymbolCache::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    fn save_symbol_cache(path: &Path, cache: &GworldSymbolCache) {
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!("Failed to create symbol cache dir: {}", e);
                return;
            }
        }
        match serde_json::to_string(cache) {
            Ok(data) => {
                if let Err(e) = fs::write(path, data) {
                    log::warn!("Failed to write symbol cache: {}", e);
                }
            }
            Err(e) => log::warn!("Failed to serialize symbol cache: {}", e),
        }
    }

    fn symbol_address(path: impl AsRef<Path>, symbol: &str) -> Result<u64> {
        let bytes = fs::read(path.as_ref()).context("게임 Mach-O 파일 읽기 실패")?;
        match Mach::parse(&bytes).context("Mach-O 파싱 실패")? {
            Mach::Binary(macho) => Self::symbol_address_from_macho(&macho, symbol),
            Mach::Fat(fat) => {
                let cputype = current_cputype();
                if let Some(arch) = fat.find_cputype(cputype).context("Fat Mach-O 탐색 실패")? {
                    let macho = MachO::parse(&bytes, arch.offset as usize)
                        .context("Mach-O arch 파싱 실패")?;
                    return Self::symbol_address_from_macho(&macho, symbol);
                }

                for i in 0..fat.narches {
                    match fat.get(i) {
                        Ok(SingleArch::MachO(macho)) => {
                            if let Ok(address) = Self::symbol_address_from_macho(&macho, symbol) {
                                return Ok(address);
                            }
                        }
                        Ok(SingleArch::Archive(_)) => {}
                        Err(e) => log::debug!("Skipping Mach-O arch: {}", e),
                    }
                }
                bail!("현재 CPU arch에서 {} 심볼을 찾지 못했습니다.", symbol)
            }
        }
    }

    fn symbol_address_from_macho(macho: &MachO<'_>, symbol: &str) -> Result<u64> {
        for item in macho.symbols() {
            let (name, nlist) = item.context("Mach-O 심볼 읽기 실패")?;
            if name == symbol {
                return Ok(nlist.n_value);
            }
        }

        bail!("{} 심볼을 찾지 못했습니다.", symbol);
    }

    fn macho_uuid(path: impl AsRef<Path>) -> Result<Option<String>> {
        let bytes = fs::read(path.as_ref()).context("게임 Mach-O 파일 읽기 실패")?;
        match Mach::parse(&bytes).context("Mach-O 파싱 실패")? {
            Mach::Binary(macho) => Ok(Self::uuid_from_macho(&macho)),
            Mach::Fat(fat) => {
                let cputype = current_cputype();
                if let Some(arch) = fat.find_cputype(cputype).context("Fat Mach-O 탐색 실패")? {
                    let macho = MachO::parse(&bytes, arch.offset as usize)
                        .context("Mach-O arch 파싱 실패")?;
                    return Ok(Self::uuid_from_macho(&macho));
                }

                for i in 0..fat.narches {
                    match fat.get(i) {
                        Ok(SingleArch::MachO(macho)) => {
                            if let Some(uuid) = Self::uuid_from_macho(&macho) {
                                return Ok(Some(uuid));
                            }
                        }
                        Ok(SingleArch::Archive(_)) => {}
                        Err(e) => log::debug!("Skipping Mach-O arch for UUID: {}", e),
                    }
                }
                Ok(None)
            }
        }
    }

    fn uuid_from_macho(macho: &MachO<'_>) -> Option<String> {
        macho.load_commands.iter().find_map(|command| {
            if let CommandVariant::Uuid(uuid_command) = &command.command {
                Some(
                    uuid_command
                        .uuid
                        .iter()
                        .map(|byte| format!("{:02x}", byte))
                        .collect::<String>(),
                )
            } else {
                None
            }
        })
    }
}

impl ProcessBackend for MacProc {
    fn is_alive(&self) -> bool {
        unsafe { kill(self.pid, 0) == 0 }
    }

    fn read_bytes(&self, address: u64, buffer: &mut [u8]) -> Result<(), NativeError> {
        self.task
            .read_bytes(address, buffer)
            .map_err(|e| NativeError::ValueReadError {
                message: e.to_string(),
            })
    }

    fn read_gworld(&self, _offset: &WuwaOffset) -> Result<u64, NativeError> {
        self.read_memory::<u64>(self.gworld_symbol_addr)
            .map_err(|e| PointerChainError {
                message: format!(
                    "_GWorld 위치 ({:X})의 주소 값을 읽지 못했습니다: {}",
                    self.gworld_symbol_addr, e
                ),
            })
    }

    fn active_offset_name(&self, offset: &WuwaOffset) -> String {
        format!("mac:{}", offset.name)
    }
}

impl MachTaskPort {
    fn open(pid: c_int) -> Result<Self> {
        let mut port = MACH_PORT_NULL;
        let kr = unsafe { task_for_pid(mach_task_self(), pid, &mut port) };
        if kr != KERN_SUCCESS || port == MACH_PORT_NULL {
            bail!(
                "게임 프로세스 권한을 얻지 못했습니다. macOS에서는 com.apple.security.cs.debugger entitlement로 앱을 서명해야 합니다. kern_return={}",
                kr
            );
        }
        Ok(Self { port })
    }

    fn dyld_info(&self) -> Result<TaskDyldInfo> {
        let mut info = TaskDyldInfo::default();
        let mut count = TASK_DYLD_INFO_COUNT;
        let kr = unsafe {
            task_info(
                self.port,
                TASK_DYLD_INFO,
                &mut info as *mut TaskDyldInfo as *mut Natural,
                &mut count,
            )
        };
        if kr != KERN_SUCCESS {
            bail!("TASK_DYLD_INFO 조회 실패: kern_return={}", kr);
        }
        Ok(info)
    }

    fn read_memory<T: Copy>(&self, address: u64) -> Result<T> {
        let mut value = std::mem::MaybeUninit::<T>::uninit();
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, std::mem::size_of::<T>())
        };
        self.read_bytes(address, buffer)
            .with_context(|| format!("원격 메모리 읽기 실패: {:X}", address))?;
        Ok(unsafe { value.assume_init() })
    }

    fn read_remote_c_string(&self, address: u64, max_len: usize) -> Result<String> {
        let mut bytes = Vec::new();
        let mut current = address;
        while bytes.len() < max_len {
            let mut chunk = [0u8; 256];
            self.read_bytes(current, &mut chunk)
                .with_context(|| format!("원격 문자열 읽기 실패: {:X}", current))?;
            if let Some(end) = chunk.iter().position(|byte| *byte == 0) {
                bytes.extend_from_slice(&chunk[..end]);
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
            bytes.extend_from_slice(&chunk);
            current += chunk.len() as u64;
        }

        bail!("원격 문자열이 너무 깁니다.");
    }

    fn read_bytes(&self, address: u64, buffer: &mut [u8]) -> Result<()> {
        if address == 0 {
            bail!("원격 메모리 주소가 0입니다.");
        }

        let mut bytes_read: MachVmSize = 0;
        let success = unsafe {
            mach_vm_read_overwrite(
                self.port,
                address,
                buffer.len() as MachVmSize,
                buffer.as_mut_ptr() as MachVmAddress,
                &mut bytes_read,
            )
        };

        if success == KERN_SUCCESS && bytes_read == buffer.len() as MachVmSize {
            Ok(())
        } else {
            bail!(
                "mach_vm_read_overwrite 실패: kern_return={}, bytes_read={}/{}",
                success,
                bytes_read,
                buffer.len()
            )
        }
    }
}

impl Drop for MachTaskPort {
    fn drop(&mut self) {
        if self.port != MACH_PORT_NULL {
            unsafe {
                mach_port_deallocate(mach_task_self(), self.port);
            }
        }
    }
}

impl Drop for MacProc {
    fn drop(&mut self) {
        log::info!("Closing Mach task port for PID {}", self.pid);
    }
}

fn current_cputype() -> u32 {
    #[cfg(target_arch = "aarch64")]
    {
        cputype::CPU_TYPE_ARM64
    }

    #[cfg(target_arch = "x86_64")]
    {
        cputype::CPU_TYPE_X86_64
    }
}

fn mach_task_self() -> MachPort {
    unsafe { mach_task_self_ }
}
