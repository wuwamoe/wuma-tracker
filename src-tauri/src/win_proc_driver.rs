/// 커널 드라이버 기반 프로세스 백엔드
///
/// WinProc(ReadProcessMemory)와 동일한 ProcessBackend trait을 구현한다.
/// native_collector.rs의 `use crate::win_proc::WinProc as PlatformProc;` 한 줄을
/// `use crate::win_proc_driver::WinProcDriver as PlatformProc;` 으로 바꾸면 교체 완료.
///
/// 드라이버와의 통신:
///   CreateFile("\\\\.\\\WumaDisplayService") → DeviceIoControl(IOCTL_*)
///
/// IOCTL/ABI는 driver/src/main.rs가 원본(source of truth)이다. 이 파일의 요청/응답
/// 레이아웃을 바꿀 때는 반드시 driver/src/main.rs의 on_* 핸들러도 함께 맞춘다.
use std::mem;
use std::path::PathBuf;
use std::ptr::null_mut;

use anyhow::{bail, Context, Result};
use winapi::{
    shared::minwindef::{DWORD, HMODULE},
    um::{
        fileapi::{CreateFileW, OPEN_EXISTING},
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        ioapiset::DeviceIoControl,
        minwinbase::STILL_ACTIVE,
        processthreadsapi::{GetExitCodeProcess, OpenProcess},
        psapi::{EnumProcessModulesEx, LIST_MODULES_DEFAULT},
        winnt::{
            FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE, HANDLE,
            PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        },
    },
};

use crate::offsets::{GWorldScanConfig, WuwaOffset};
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;
use crate::types::NativeError::{PointerChainError, ValueReadError};

// ── IOCTL 코드 (driver/src/main.rs의 `ctl(func) = (0x22 << 16) | (func << 2)`와 동일) ──

const fn ctl(func: u32) -> u32 {
    (0x22 << 16) | (func << 2)
}
const IOCTL_AUTH: u32 = ctl(0x800);
const IOCTL_SET_CONFIG: u32 = ctl(0x801);
const IOCTL_GET_LOCATION: u32 = ctl(0x802);
const IOCTL_PATTERN_SEARCH: u32 = ctl(0x804);

// ── 드라이버 디바이스 경로 ────────────────────────────────────────────────────

const DEVICE_PATH: &str = r"\\.\WumaDisplayService";
const MAX_CHAIN: usize = 16;

// ── 요청/응답 구조체 (driver/src/main.rs의 온-와이어 레이아웃과 바이트 단위로 일치) ──

#[repr(C)]
struct AuthReq {
    pid: u32,
    _pad: u32,
}

#[repr(C)]
struct AuthResp {
    token: u64,
}

/// driver::on_set_config 의 요청 레이아웃:
///   [0..8] token, [8..12] pid, [12] chain_len, [13] origin_chain_len, [14..16] pad
///   [16..24] base_addr, [24..32] anchor_rva, [32..40] transform_offset, [40..48] origin_offset
///   [48..] chain[chain_len*8] ++ origin_chain[origin_chain_len*8]
#[repr(C)]
struct SetConfigReq {
    token: u64,
    pid: u32,
    chain_len: u8,
    origin_chain_len: u8,
    _pad: u16,
    base_addr: u64,
    anchor_rva: u64,
    transform_offset: u64,
    origin_offset: u64,
    chain: [u64; MAX_CHAIN],
    origin_chain: [u64; MAX_CHAIN],
}

#[repr(C)]
struct GetLocationReq {
    token: u64,
}

#[repr(C)]
struct GetLocationResp {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
    /// 0=성공, 1~=실패한 체인 단계 (진단용)
    stage: u8,
    _pad: [u8; 3],
}

#[repr(C)]
struct PatternSearchReq {
    token: u64,
    target_pid: u32,
    _pad: u32,
    base_address: u64,
    image_size: u32,
    prefix_len: u8,
    suffix_len: u8,
    _pad2: u16,
    prefix: [u8; 16],
    /// 0xFF = wildcard
    suffix: [u8; 32],
}

#[repr(C)]
struct PatternSearchResp {
    gworld_rva: u64,
}

// ── 포인터 체인 → 커널 SET_CONFIG 페이로드 변환 ──────────────────────────────
//
// process_backend::read_player_info (소프트웨어 경로)가 밟는 체인과 동일하게 맞춘다:
//   anchor(GWorld) -> +uworld_owninggameinstance -> +ugameinstance_localplayers
//   -> +0(LocalPlayer[0]) -> +uplayer_playercontroller
//   -> +aplayercontroller_acknowlegedpawn -> +aactor_rootcomponent
//   -> (+uscenecomponent_componenttoworld) FTransform 읽기
// origin 체인은 GWorld에서 별도로: +uworld_persistentlevel -> (+ulevel_lastworldorigin) FIntVector 읽기

fn build_chain(offset: &WuwaOffset) -> ([u64; MAX_CHAIN], u8, u64, [u64; MAX_CHAIN], u8, u64) {
    let mut chain = [0u64; MAX_CHAIN];
    chain[0] = offset.uworld_owninggameinstance;
    chain[1] = offset.ugameinstance_localplayers;
    chain[2] = 0; // LocalPlayer[0]
    chain[3] = offset.uplayer_playercontroller;
    chain[4] = offset.aplayercontroller_acknowlegedpawn;
    chain[5] = offset.aactor_rootcomponent;
    let chain_len = 6u8;
    let transform_offset = offset.uscenecomponent_componenttoworld;

    let mut origin_chain = [0u64; MAX_CHAIN];
    origin_chain[0] = offset.uworld_persistentlevel;
    let origin_chain_len = 1u8;
    let origin_offset = offset.ulevel_lastworldorigin;

    (chain, chain_len, transform_offset, origin_chain, origin_chain_len, origin_offset)
}

// ── WinProcDriver ─────────────────────────────────────────────────────────────

pub struct WinProcDriver {
    /// 드라이버 디바이스 핸들
    device: HANDLE,
    /// 게임 프로세스 핸들 (생존 확인 전용, VM_READ 권한 없음)
    proc_handle: HANDLE,
    /// 세션 토큰 (IOCTL_AUTH로 발급)
    token: u64,
    /// 게임 프로세스 PID
    pid: u32,
    /// 게임 모듈 베이스 주소
    pub base_addr: u64,
    /// GWorld RVA (0이면 폴백 사용)
    gworld_rva: u64,
    scan_config: GWorldScanConfig,
}

impl WinProcDriver {
    /// WinProc::new와 동일한 시그니처 — native_collector.rs 무수정 교체용
    pub fn new(name: &str, _cache_dir: PathBuf, scan_config: Option<GWorldScanConfig>) -> Result<Self> {
        let scan_config = scan_config.unwrap_or_default();

        // 1. 드라이버 디바이스 오픈
        let device = open_device()
            .context("드라이버 디바이스를 열지 못했습니다. 드라이버가 로드되어 있는지 확인하세요.")?;

        // 2. 인증 (세션 토큰 발급)
        let token = auth(device).context("드라이버 인증 실패")?;
        log::info!("[WinProcDriver] 드라이버 인증 성공, token=0x{:X}", token);

        // 3. 게임 PID 찾기
        let pid =
            find_pid_by_name(name).with_context(|| "게임이 실행 중이 아닙니다.".to_string())?;

        // 4. 베이스 주소 획득 — 유저모드 모듈 목록으로 조회 (드라이버는 GET_PROCESS_BASE를
        //    제공하지 않는다: 모듈 나열은 안티치트 후킹 대상이 아니라 커널을 거칠 필요가 없다)
        let base_addr = get_base_address(pid).context("게임 베이스 주소를 가져오지 못했습니다.")?;
        log::info!("[WinProcDriver] PID={} Base=0x{:X}", pid, base_addr);

        // 생존 확인 전용 핸들 — IOCTL_READ_MEMORY가 없으므로 드라이버를 거치지 않고
        // 유저모드에서 직접 프로세스 종료 여부를 확인한다.
        let proc_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if proc_handle.is_null() {
            bail!("생존 확인용 프로세스 핸들을 얻지 못했습니다: {}", std::io::Error::last_os_error());
        }

        // 5. GWorld RVA 스캔 (드라이버 경유)
        let gworld_rva = if scan_config.enabled {
            match scan_gworld(device, token, pid, base_addr, &scan_config) {
                Ok(rva) => {
                    log::info!("[WinProcDriver] GWorld RVA=0x{:X}", rva);
                    rva
                }
                Err(e) => {
                    log::warn!("[WinProcDriver] GWorld 스캔 실패, 폴백 사용: {}", e);
                    0
                }
            }
        } else {
            log::info!("[WinProcDriver] GWorld 스캔 비활성화, 폴백 사용");
            0
        };

        Ok(Self {
            device,
            proc_handle,
            token,
            pid,
            base_addr,
            gworld_rva,
            scan_config,
        })
    }
}

impl ProcessBackend for WinProcDriver {
    fn is_alive(&self) -> bool {
        let mut exit_code: DWORD = 0;
        unsafe {
            GetExitCodeProcess(self.proc_handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as DWORD
        }
    }

    /// IOCTL_READ_MEMORY를 제거했으므로 이 백엔드는 범용 임의 주소 읽기를 지원하지 않는다.
    /// 좌표 조회는 항상 get_player_info(fast path, GET_LOCATION 1회)를 통해서만 이뤄진다.
    fn read_bytes(&self, address: u64, _buffer: &mut [u8]) -> Result<(), NativeError> {
        Err(ValueReadError {
            message: format!(
                "WinProcDriver는 범용 메모리 읽기를 지원하지 않습니다 (addr={:X}). get_player_info(fast path)만 사용하세요.",
                address
            ),
        })
    }

    fn gworld_ready(&self) -> bool {
        self.gworld_rva != 0
    }

    fn active_offset_name(&self, offset: &WuwaOffset) -> String {
        if self.gworld_rva != 0 {
            format!("drv:{:X}", self.gworld_rva)
        } else {
            format!("drv:fb:{}", offset.name)
        }
    }

    fn rescan_gworld(&mut self) {
        if !self.scan_config.enabled {
            return;
        }
        match scan_gworld(self.device, self.token, self.pid, self.base_addr, &self.scan_config) {
            Ok(rva) => {
                log::info!("[WinProcDriver] GWorld 재스캔 성공: RVA=0x{:X}", rva);
                self.gworld_rva = rva;
            }
            Err(e) => {
                log::warn!("[WinProcDriver] GWorld 재스캔 실패: {}", e);
                self.gworld_rva = 0;
            }
        }
    }

    fn read_gworld(&self, offset: &WuwaOffset) -> Result<u64, NativeError> {
        let (rva, source) = if self.gworld_rva != 0 {
            (self.gworld_rva, "scan")
        } else {
            (offset.global_gworld, "fb")
        };
        let target = self.base_addr + rva;
        self.read_memory::<u64>(target).map_err(|e| PointerChainError {
            message: format!("gworld@{:X} rva={:X}[{}]: {}", target, rva, source, e),
        })
    }

    /// fast path: 포인터 체인 전체를 드라이버에서 1회 IOCTL로 처리
    /// select_player_info에서 slow path(개별 read_bytes)보다 먼저 시도된다
    fn get_player_info(&self, offset: &WuwaOffset) -> Option<Result<crate::types::PlayerInfo, NativeError>> {
        // SET_CONFIG가 아직 전송되지 않았다면(최초 호출 또는 재스캔 직후) 먼저 보낸다.
        // &self 라서 config_ready를 갱신할 수 없으므로, 매 호출마다 안전하게 다시 보낸다.
        // (SET_CONFIG는 드라이버 쪽에서 저비용 연산이라 매 폴링(500ms)마다 보내도 무방하다.)
        let anchor_rva = if self.gworld_rva != 0 {
            self.gworld_rva
        } else {
            offset.global_gworld
        };
        let (chain, chain_len, transform_offset, origin_chain, origin_chain_len, origin_offset) =
            build_chain(offset);
        let cfg_req = SetConfigReq {
            token: self.token,
            pid: self.pid,
            chain_len,
            origin_chain_len,
            _pad: 0,
            base_addr: self.base_addr,
            anchor_rva,
            transform_offset,
            origin_offset,
            chain,
            origin_chain,
        };
        let cfg_ok = ioctl(
            self.device,
            IOCTL_SET_CONFIG,
            &cfg_req as *const _ as *const _,
            mem::size_of::<SetConfigReq>() as DWORD,
            null_mut(),
            0,
        );
        if !cfg_ok {
            return Some(Err(ValueReadError {
                message: format!("SET_CONFIG 실패: {}", std::io::Error::last_os_error()),
            }));
        }

        let req = GetLocationReq { token: self.token };
        let mut resp = GetLocationResp {
            x: 0.0, y: 0.0, z: 0.0,
            pitch: 0.0, yaw: 0.0, roll: 0.0,
            stage: 0,
            _pad: [0; 3],
        };

        let ok = ioctl(
            self.device,
            IOCTL_GET_LOCATION,
            &req as *const _ as *const _,
            mem::size_of::<GetLocationReq>() as DWORD,
            &mut resp as *mut _ as *mut _,
            mem::size_of::<GetLocationResp>() as DWORD,
        );

        if !ok {
            return Some(Err(ValueReadError {
                message: format!(
                    "IOCTL_GET_LOCATION 실패: {}",
                    std::io::Error::last_os_error()
                ),
            }));
        }

        if resp.stage != 0 {
            let stage_name = match resp.stage {
                1 => "GWorld(anchor)",
                2 => "OwningGameInstance",
                3 => "TArray<LocalPlayers>",
                4 => "LocalPlayer[0]",
                5 => "PlayerController",
                6 => "AcknowledgedPawn",
                7 => "RootComponent",
                8 => "FTransform",
                9 => "PersistentLevel",
                10 => "WorldOrigin",
                _ => "unknown",
            };
            return Some(Err(PointerChainError {
                message: format!("드라이버 체인 실패 stage={}({})", resp.stage, stage_name),
            }));
        }

        Some(Ok(crate::types::PlayerInfo {
            x: resp.x, y: resp.y, z: resp.z,
            pitch: resp.pitch, yaw: resp.yaw, roll: resp.roll,
        }))
    }
}

impl Drop for WinProcDriver {
    fn drop(&mut self) {
        if self.device != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.device); }
        }
        if !self.proc_handle.is_null() && self.proc_handle != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.proc_handle); }
        }
    }
}

unsafe impl Send for WinProcDriver {}

// ── 드라이버 통신 헬퍼 ────────────────────────────────────────────────────────

fn open_device() -> Result<HANDLE> {
    let path_wide: Vec<u16> = DEVICE_PATH
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        bail!(
            "드라이버 디바이스 열기 실패: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(handle)
}

fn auth(device: HANDLE) -> Result<u64> {
    let req = AuthReq {
        pid: std::process::id(),
        _pad: 0,
    };
    let mut resp = AuthResp { token: 0 };

    let ok = ioctl(
        device,
        IOCTL_AUTH,
        &req as *const _ as *const _,
        mem::size_of::<AuthReq>() as DWORD,
        &mut resp as *mut _ as *mut _,
        mem::size_of::<AuthResp>() as DWORD,
    );

    if !ok || resp.token == 0 {
        bail!("인증 IOCTL 실패: {}", std::io::Error::last_os_error());
    }
    Ok(resp.token)
}

/// 게임 메인 모듈 베이스 주소를 유저모드에서 직접 조회한다.
/// (모듈 나열은 안티치트가 보호하는 대상이 아니므로 드라이버를 거칠 필요가 없다)
fn get_base_address(pid: u32) -> Result<u64> {
    unsafe {
        let handle = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid);
        if handle.is_null() {
            bail!("프로세스 열기 실패: {}", std::io::Error::last_os_error());
        }

        let mut h_mod: HMODULE = null_mut();
        let mut cb_needed: DWORD = 0;
        let ok = EnumProcessModulesEx(
            handle,
            &mut h_mod,
            mem::size_of::<HMODULE>() as DWORD,
            &mut cb_needed,
            LIST_MODULES_DEFAULT,
        );
        CloseHandle(handle);

        if ok == 0 {
            bail!("모듈 목록 조회 실패: {}", std::io::Error::last_os_error());
        }
        Ok(h_mod as u64)
    }
}

fn scan_gworld(
    device: HANDLE,
    token: u64,
    pid: u32,
    base_address: u64,
    config: &GWorldScanConfig,
) -> Result<u64> {
    let prefix = parse_hex_bytes(&config.prefix);
    let suffix = parse_wildcard_bytes(&config.suffix);

    if prefix.is_empty() || prefix.len() > 16 {
        bail!("prefix 길이 오류: {}", prefix.len());
    }
    if suffix.len() > 32 {
        bail!("suffix 길이 오류: {}", suffix.len());
    }

    let mut req = PatternSearchReq {
        token,
        target_pid: pid,
        _pad: 0,
        base_address,
        image_size: 0, // 드라이버가 PE 헤더에서 직접 읽음
        prefix_len: prefix.len() as u8,
        suffix_len: suffix.len() as u8,
        _pad2: 0,
        prefix: [0u8; 16],
        suffix: [0xFF_u8; 32], // 기본값 0xFF = wildcard
    };
    req.prefix[..prefix.len()].copy_from_slice(&prefix);
    req.suffix[..suffix.len()].copy_from_slice(&suffix);

    let mut resp = PatternSearchResp { gworld_rva: 0 };

    let ok = ioctl(
        device,
        IOCTL_PATTERN_SEARCH,
        &req as *const _ as *const _,
        mem::size_of::<PatternSearchReq>() as DWORD,
        &mut resp as *mut _ as *mut _,
        mem::size_of::<PatternSearchResp>() as DWORD,
    );

    if !ok || resp.gworld_rva == 0 {
        bail!("패턴 스캔 IOCTL 실패 또는 패턴 없음: {}", std::io::Error::last_os_error());
    }
    Ok(resp.gworld_rva)
}

/// DeviceIoControl 래퍼 — bool 반환
fn ioctl(
    device: HANDLE,
    code: u32,
    in_buf: *const std::ffi::c_void,
    in_size: DWORD,
    out_buf: *mut std::ffi::c_void,
    out_size: DWORD,
) -> bool {
    let mut returned: DWORD = 0;
    unsafe {
        DeviceIoControl(
            device,
            code,
            in_buf as *mut winapi::ctypes::c_void,
            in_size,
            out_buf as *mut winapi::ctypes::c_void,
            out_size,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    }
}

// ── 유틸리티 ─────────────────────────────────────────────────────────────────

/// "48 8B 1D" → vec![0x48, 0x8B, 0x1D]
fn parse_hex_bytes(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect()
}

/// "48 85 DB 74 ?? 41" → vec![0x48, 0x85, 0xDB, 0x74, 0xFF, 0x41]
/// (드라이버 쪽에서 0xFF를 wildcard로 해석)
fn parse_wildcard_bytes(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| if b == "??" { 0xFF } else { u8::from_str_radix(b, 16).unwrap_or(0xFF) })
        .collect()
}

/// 프로세스 이름으로 PID 찾기 (WinProc의 find_pid_by_name과 동일)
fn find_pid_by_name(name: &str) -> Option<u32> {
    use std::ffi::CStr;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut pe: PROCESSENTRY32 = mem::zeroed();
        pe.dwSize = mem::size_of::<PROCESSENTRY32>() as u32;

        if Process32First(snap, &mut pe) != 0 {
            loop {
                let exe = CStr::from_ptr(pe.szExeFile.as_ptr()).to_string_lossy();
                if exe == name {
                    CloseHandle(snap);
                    return Some(pe.th32ProcessID);
                }
                if Process32Next(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        None
    }
}
