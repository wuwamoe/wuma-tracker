use std::path::{Path, PathBuf};
use std::{ffi::CStr, fs, mem, ptr::null_mut};

use crate::offsets::WuwaOffset;
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use winapi::um::minwinbase::STILL_ACTIVE;
use winapi::um::processthreadsapi::GetExitCodeProcess;
use winapi::{
    ctypes::c_void,
    shared::minwindef::{DWORD, HMODULE},
    um::{
        handleapi::CloseHandle,
        memoryapi::ReadProcessMemory,
        processthreadsapi::OpenProcess,
        psapi::{EnumProcessModulesEx, GetModuleFileNameExW, LIST_MODULES_DEFAULT},
        tlhelp32::{
            CreateToolhelp32Snapshot, PROCESSENTRY32, Process32First, Process32Next,
            TH32CS_SNAPPROCESS,
        },
        winnt::{HANDLE, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
    },
};

// ── GWorld 패턴 ───────────────────────────────────────────────────────────────
//   MOV  RBX, [RIP+?] 48 8B 1D ?? ?? ?? ??
//   TEST RBX, RBX     48 85 DB
//   JZ   +??          74 ??   (오프셋은 버전마다 다를 수 있으므로 wildcard)
//   MOV  R8B, 1       41 B0 01
//
// disp32는 오프셋 3에 위치, MOV 명령어 길이 = 7바이트
const GWORLD_PREFIX: [u8; 3] = [0x48, 0x8B, 0x1D];
const GWORLD_SUFFIX: [u8; 3] = [0x48, 0x85, 0xDB]; // TEST RBX, RBX
const GWORLD_TAIL: [u8; 3] = [0x41, 0xB0, 0x01];   // MOV R8B, 1
// 패턴 구조: PREFIX(3) + disp32(4) + SUFFIX(3) + JZ_op(1) + JZ_rel(1) + TAIL(3) = 15바이트
const PATTERN_INSTR_LEN: usize = 7;  // MOV RBX, [RIP+disp] 길이
const PATTERN_TOTAL_LEN: usize = 15; // 전체 매칭 길이

// .pdata 스캔 배치 파라미터
const BATCH_GAP: u64 = 4096;           // 이 이상 VA 갭이 나면 새 배치
const MAX_BATCH_SIZE: usize = 256 * 1024; // 배치당 최대 읽기 크기

const CACHE_FILE: &str = "win_gworld_scan_cache.json";

// ── 캐시 구조체 ───────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct GworldScanCache {
    entries: Vec<GworldScanCacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct GworldScanCacheEntry {
    exe_path: String,
    pe_timestamp: u32,
    size_of_image: u32,
    gworld_rva: u64,
}

// ── WinProc ───────────────────────────────────────────────────────────────────

pub struct WinProc {
    pid: u32,
    pub base_addr: u64,
    handle: HANDLE,
    gworld_rva: u64,
    cache_dir: PathBuf,
}

impl WinProc {
    pub fn new(name: &str, cache_dir: PathBuf) -> Result<Self> {
        unsafe {
            let pid = Self::find_pid_by_name(name)
                .with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;

            let handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid);
            if handle.is_null() {
                bail!(
                    "게임에 연결하지 못했습니다. OS Error: {}",
                    std::io::Error::last_os_error()
                );
            }

            let mut h_mod: HMODULE = null_mut();
            let mut cb_needed = 0;
            if EnumProcessModulesEx(
                handle,
                &mut h_mod,
                mem::size_of::<HMODULE>() as DWORD,
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

            let base_addr = h_mod as u64;

            let gworld_rva = match find_gworld_rva_with_cache(handle, base_addr, &cache_dir) {
                Ok(rva) => rva,
                Err(e) => {
                    log::warn!("GWorld 스캔 실패, 오프셋 폴백 사용: {}", e);
                    0
                }
            };

            log::info!(
                "Process '{}' connected! PID: {}, Base: {:X}, GWorld RVA: {}",
                name, pid, base_addr,
                if gworld_rva != 0 { format!("{:X}", gworld_rva) } else { "폴백".to_string() }
            );

            Ok(WinProc { pid, base_addr, handle, gworld_rva, cache_dir })
        }
    }

    unsafe fn find_pid_by_name(name: &str) -> Option<u32> {
        let h_process_snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if h_process_snap.is_null() {
            return None;
        }

        let mut pe32: PROCESSENTRY32 = mem::zeroed();
        pe32.dwSize = mem::size_of::<PROCESSENTRY32>() as u32;

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
                        address, bytes_read, buffer.len()
                    ),
                })
            }
        }
    }

    fn gworld_ready(&self) -> bool {
        self.gworld_rva != 0
    }

    fn rescan_gworld(&mut self) {
        match scan_gworld_rva(self.handle, self.base_addr) {
            Ok(rva) => {
                log::info!("GWorld 재스캔 성공: RVA 0x{:X}", rva);
                self.gworld_rva = rva;
                // 캐시 갱신
                if let Ok((timestamp, size_of_image, _, _)) =
                    read_pe_exception_dir(self.handle, self.base_addr)
                {
                    let exe_path = get_module_path(self.handle).unwrap_or_default();
                    let cache_path = self.cache_dir.join(CACHE_FILE);
                    let mut cache = load_cache(&cache_path);
                    cache.entries.retain(|e| e.exe_path != exe_path);
                    cache.entries.push(GworldScanCacheEntry {
                        exe_path,
                        pe_timestamp: timestamp,
                        size_of_image,
                        gworld_rva: rva,
                    });
                    save_cache(&cache_path, &cache);
                }
            }
            Err(e) => {
                log::warn!("GWorld 재스캔 실패, 폴백 유지: {}", e);
                self.gworld_rva = 0;
            }
        }
    }

    fn read_gworld(&self, offset: &WuwaOffset) -> Result<u64, NativeError> {
        let rva = if self.gworld_rva != 0 { self.gworld_rva } else { offset.global_gworld };
        let target = self.base_addr + rva;
        self.read_memory::<u64>(target).map_err(|e| PointerChainError {
            message: format!("GWorld 위치 ({:X})의 주소 값을 읽지 못했습니다: {}", target, e),
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

// ── PE 헤더 파싱 ──────────────────────────────────────────────────────────────

fn rpm_u32(handle: HANDLE, addr: u64) -> Option<u32> {
    let mut buf = [0u8; 4];
    let mut br = 0usize;
    let ok = unsafe {
        ReadProcessMemory(handle, addr as _, buf.as_mut_ptr() as _, 4, &mut br) != 0 && br == 4
    };
    ok.then(|| u32::from_le_bytes(buf))
}

fn rpm_buf(handle: HANDLE, addr: u64, buf: &mut [u8]) -> bool {
    let mut br = 0usize;
    unsafe {
        ReadProcessMemory(handle, addr as _, buf.as_mut_ptr() as _, buf.len(), &mut br) != 0
            && br == buf.len()
    }
}

/// PE Optional Header 파싱 → (pe_timestamp, size_of_image, pdata_rva, pdata_size)
fn read_pe_exception_dir(handle: HANDLE, base: u64) -> Result<(u32, u32, u32, u32)> {
    // DOS header: e_lfanew at +0x3C
    let e_lfanew = rpm_u32(handle, base + 0x3C).context("e_lfanew 읽기 실패")? as u64;
    let pe = base + e_lfanew;

    // IMAGE_FILE_HEADER.TimeDateStamp: PE sig(4) + Machine(2) + NumberOfSections(2) = +8
    let timestamp = rpm_u32(handle, pe + 8).context("TimeDateStamp 읽기 실패")?;

    // IMAGE_OPTIONAL_HEADER64: PE sig(4) + FILE_HEADER(20) = +24
    let opt = pe + 24;

    // SizeOfImage: OptionalHeader+56
    let size_of_image = rpm_u32(handle, opt + 56).context("SizeOfImage 읽기 실패")?;

    // DataDirectory[3] (EXCEPTION = .pdata): OptionalHeader+112 + 3*8
    let dd = opt + 112 + 24;
    let pdata_rva = rpm_u32(handle, dd).context(".pdata RVA 읽기 실패")?;
    let pdata_size = rpm_u32(handle, dd + 4).context(".pdata size 읽기 실패")?;

    Ok((timestamp, size_of_image, pdata_rva, pdata_size))
}

fn get_module_path(handle: HANDLE) -> Result<String> {
    let mut buf = vec![0u16; 32768];
    let len = unsafe {
        GetModuleFileNameExW(handle, null_mut(), buf.as_mut_ptr(), buf.len() as u32)
    };
    if len == 0 {
        bail!("GetModuleFileNameExW 실패: {}", std::io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(&buf[..len as usize]))
}

// ── .pdata 기반 GWorld RVA 스캔 ───────────────────────────────────────────────

fn scan_gworld_rva(handle: HANDLE, base: u64) -> Result<u64> {
    let (_, size_of_image, pdata_rva, pdata_size) = read_pe_exception_dir(handle, base)?;

    if pdata_rva == 0 || pdata_size < 12 {
        bail!(".pdata 섹션을 찾지 못했습니다.");
    }

    // .pdata 전체 읽기 (RUNTIME_FUNCTION: BeginAddress(4) + EndAddress(4) + UnwindInfo(4))
    let entry_count = pdata_size as usize / 12;
    let mut pdata = vec![0u8; entry_count * 12];
    if !rpm_buf(handle, base + pdata_rva as u64, &mut pdata) {
        bail!(".pdata 읽기 실패");
    }

    log::info!("GWorld 스캔: .pdata 함수 {} 개", entry_count);

    // (BeginAddress, EndAddress) 추출 (유효 범위만)
    let mut funcs: Vec<(u32, u32)> = (0..entry_count)
        .map(|i| {
            let b = u32::from_le_bytes(pdata[i * 12..i * 12 + 4].try_into().unwrap());
            let e = u32::from_le_bytes(pdata[i * 12 + 4..i * 12 + 8].try_into().unwrap());
            (b, e)
        })
        .filter(|&(b, e)| b > 0 && e > b && e <= size_of_image)
        .collect();
    funcs.sort_unstable_by_key(|&(b, _)| b);

    // 인접 함수들을 배치로 묶어 ReadProcessMemory 최소화
    // 패턴은 함수 body 어디에나 있을 수 있으므로 EndAddress까지 읽음
    let mut i = 0;
    while i < funcs.len() {
        let batch_start = funcs[i].0 as u64;
        let mut batch_end = funcs[i].1 as u64;
        let mut j = i + 1;

        while j < funcs.len() {
            let gap = funcs[j].0 as u64 - batch_end;
            let new_end = funcs[j].1 as u64;
            if gap > BATCH_GAP || new_end - batch_start > MAX_BATCH_SIZE as u64 {
                break;
            }
            batch_end = new_end;
            j += 1;
        }

        let read_size = (batch_end - batch_start) as usize;
        let mut buf = vec![0u8; read_size];

        if rpm_buf(handle, base + batch_start, &mut buf) {
            // 배치 내 모든 바이트에서 패턴 탐색
            let search_end = buf.len().saturating_sub(PATTERN_TOTAL_LEN - 1);
            for off in 0..search_end {
                if buf[off..off + 3] != GWORLD_PREFIX {
                    continue;
                }
                if buf[off + 7..off + 10] != GWORLD_SUFFIX {
                    continue;
                }
                if buf[off + 10] != 0x74 {
                    continue;
                }
                if buf[off + 12..off + 15] != GWORLD_TAIL {
                    continue;
                }

                let disp = i32::from_le_bytes(buf[off + 3..off + 7].try_into().unwrap());
                let instr_rva = batch_start + off as u64;
                let gworld_rva =
                    ((instr_rva as i64) + PATTERN_INSTR_LEN as i64 + disp as i64) as u64;

                if gworld_rva > 0 && gworld_rva < size_of_image as u64 {
                    log::info!(
                        "GWorld RVA 발견: 0x{:X} (명령어 RVA 0x{:X}, disp {:+#X})",
                        gworld_rva, instr_rva, disp
                    );
                    return Ok(gworld_rva);
                }
            }
        }

        i = j;
    }

    bail!("GWorld 패턴을 찾지 못했습니다.")
}

// ── 캐시 ─────────────────────────────────────────────────────────────────────

fn load_cache(path: &Path) -> GworldScanCache {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cache(path: &Path, cache: &GworldScanCache) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string(cache) {
        let _ = fs::write(path, data);
    }
}

fn find_gworld_rva_with_cache(handle: HANDLE, base: u64, cache_dir: &Path) -> Result<u64> {
    let exe_path = get_module_path(handle).unwrap_or_default();
    let (timestamp, size_of_image, _, _) = read_pe_exception_dir(handle, base)?;

    let cache_path = cache_dir.join(CACHE_FILE);
    let mut cache = load_cache(&cache_path);

    if let Some(entry) = cache.entries.iter().find(|e| {
        e.exe_path == exe_path
            && e.pe_timestamp == timestamp
            && e.size_of_image == size_of_image
    }) {
        log::info!("캐시된 GWorld RVA 사용: 0x{:X}", entry.gworld_rva);
        return Ok(entry.gworld_rva);
    }

    log::info!("GWorld RVA 캐시 미스 → .pdata 스캔 시작");
    let gworld_rva = scan_gworld_rva(handle, base)?;

    cache.entries.retain(|e| e.exe_path != exe_path);
    cache.entries.push(GworldScanCacheEntry {
        exe_path,
        pe_timestamp: timestamp,
        size_of_image,
        gworld_rva,
    });
    save_cache(&cache_path, &cache);

    Ok(gworld_rva)
}
