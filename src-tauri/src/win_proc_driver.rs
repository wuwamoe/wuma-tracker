/// 커널 드라이버 기반 프로세스 백엔드
///
/// `ProcessBackend`의 유일한 액션인 `poll()`을 헬퍼 IPC 1회 요청으로 구현한다.
/// native_collector.rs의 `use crate::win_proc::WinProc as PlatformProc;` 한 줄을
/// `use crate::win_proc_driver::WinProcDriver as PlatformProc;` 으로 바꾸면 교체 완료.
///
/// 이 앱은 드라이버를 직접 열지 않는다. 모든 좌표 조회는 헬퍼 서비스의 named pipe
/// (`\\.\pipe\WumaTrackerHelper`)를 통해서만 이뤄지며, 헬퍼가 실제로
/// `\\.\WumaDisplayService`를 열고 IOCTL_GET_LOCATION을 호출한다. 요청/응답 온-와이어
/// 레이아웃은 `driver/shared/ioctls.rs`가 원본(source of truth)이다.
///
/// 프로세스 attach 자체도 드라이버/헬퍼가 매 호출마다 내부적으로 처리하므로
/// (이름으로 프로세스를 찾아 붙는 것까지 IOCTL 한 번에 포함), 이 백엔드는 별도의
/// "붙어있는 핸들"을 들고 있지 않는다 — `poll()`만 반복 호출하면 된다.
use std::io::{Read, Write};
use std::mem;
use std::path::PathBuf;

use anyhow::{bail, Result};
use winapi::um::{
    fileapi::{CreateFileW, OPEN_EXISTING},
    handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
    winnt::{GENERIC_READ, GENERIC_WRITE, HANDLE},
};

use crate::offsets::{GWorldScanConfig, WuwaOffset};
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;

const PIPE_NAME: &str = r"\\.\pipe\WumaTrackerHelper";
const REQ_GET_LOCATION: u32 = 1;

/// 드라이버 체인은 오프셋 파일 버전과 무관하게 하드코딩되어 있으므로,
/// 어느 오프셋이 로드됐는지와 상관없이 이 이름 하나로 고정한다.
const FIXED_OFFSET_NAME: &str = "v2";

const STAGE_DRIVER_UNAVAILABLE: u8 = 0xFF;
const STAGE_TARGET_MISSING: u8 = 0xFE;

#[repr(C)]
struct GetLocationResp {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
    /// 0=성공, 0xFF=헬퍼가 드라이버를 못 엶, 0xFE=대상 프로세스 없음, 그 외=포인터 체인 실패 단계
    stage: u8,
    _pad: [u8; 3],
}

pub struct WinProcDriver;

impl WinProcDriver {
    /// WinProc::new와 동일한 시그니처 — native_collector.rs 무수정 교체용.
    /// scan_config는 더 이상 사용되지 않는다: World Anchor/포인터 체인은 드라이버에
    /// 하드코딩되어 있고, 이 앱은 런타임에 그것을 바꿀 방법이 없다.
    pub fn new(name: &str, _cache_dir: PathBuf, _scan_config: Option<GWorldScanConfig>) -> Result<Self> {
        // attach 버튼을 눌렀는데 게임이 아예 안 떠있으면 즉시 실패로 피드백한다.
        // (이후의 좌표 조회는 헬퍼/드라이버가 매번 알아서 프로세스를 찾으므로 이 확인은
        // 여기서 1회, "붙기 시도" 자체의 성공/실패 판정용으로만 쓰인다.)
        if find_pid_by_name(name).is_none() {
            bail!("게임이 실행 중이 아닙니다.");
        }
        Ok(Self)
    }
}

impl ProcessBackend for WinProcDriver {
    /// 오프셋 인자는 "v2"가 로드되어 있는지 확인하는 용도로만 쓰인다.
    /// 실제 포인터 체인 내용은 드라이버 내부에 하드코딩되어 있어 참조하지 않는다.
    fn poll(&mut self, offsets: &[WuwaOffset]) -> Result<crate::types::PlayerInfo, NativeError> {
        if !offsets.iter().any(|o| o.name == FIXED_OFFSET_NAME) {
            return Err(NativeError::PointerChainError {
                message: format!("오프셋 '{}'를 찾을 수 없습니다.", FIXED_OFFSET_NAME),
            });
        }
        call_helper_get_location()
    }

    fn diagnostics(&self) -> String {
        format!("helper:hardcoded-chain[{}]", FIXED_OFFSET_NAME)
    }
}

// ── 헬퍼 IPC ─────────────────────────────────────────────────────────────────

fn call_helper_get_location() -> Result<crate::types::PlayerInfo, NativeError> {
    let pipe = open_helper_pipe()?;
    let mut file = unsafe { <std::fs::File as std::os::windows::io::FromRawHandle>::from_raw_handle(pipe as _) };

    if file.write_all(&REQ_GET_LOCATION.to_le_bytes()).is_err() {
        return Err(NativeError::HelperUnavailable);
    }

    let mut buf = [0u8; mem::size_of::<GetLocationResp>()];
    if file.read_exact(&mut buf).is_err() {
        return Err(NativeError::HelperUnavailable);
    }
    // file은 raw handle을 소유(drop 시 자동 CloseHandle)하므로 별도 정리가 필요 없다.

    let resp = unsafe { &*(buf.as_ptr() as *const GetLocationResp) };

    // 프로세스를 못 찾았다는 것만 "연결 해제"로 취급한다. 그 외(헬퍼/드라이버 불가,
    // 포인터 체인 실패)는 프로세스는 여전히 붙어 있는 것으로 가정하는 일시적 오류다.
    if resp.stage == STAGE_TARGET_MISSING {
        return Err(NativeError::ProcessTerminated);
    }
    if resp.stage == STAGE_DRIVER_UNAVAILABLE {
        return Err(NativeError::DriverUnavailable);
    }
    if resp.stage != 0 {
        return Err(NativeError::PointerChainError {
            message: describe_driver_stage(resp.stage),
        });
    }

    Ok(crate::types::PlayerInfo {
        x: resp.x,
        y: resp.y,
        z: resp.z,
        pitch: resp.pitch,
        yaw: resp.yaw,
        roll: resp.roll,
    })
}

/// 드라이버의 stage 코드는 `driver/src/main.rs`의 `read_player_world_position`
/// 실패 지점과 1:1로 대응한다(포인터 체인 길이 6 기준):
/// 1: 모듈 베이스/World Anchor 탐색 실패, 2~7: 포인터 체인 1~6단계 역참조 실패,
/// 8: 최종 좌표(FRelativeTransform) 읽기 실패.
fn describe_driver_stage(stage: u8) -> String {
    match stage {
        1 => "메인 모듈 베이스 또는 World Anchor 탐색 실패".to_string(),
        2..=7 => format!("포인터 체인 {}단계 역참조 실패", stage - 1),
        8 => "좌표 트랜스폼(FRelativeTransform) 읽기 실패".to_string(),
        other => format!("드라이버 체인 실패 (알 수 없는 stage={})", other),
    }
}

/// 헬퍼의 named pipe를 연다. 헬퍼 프로세스가 없거나(ENOENT류), 접근이 거부된 경우
/// (헬퍼가 이 앱 계정 외의 접근을 차단) 각각 다른 오류로 구분해 보고한다.
fn open_helper_pipe() -> Result<HANDLE, NativeError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let path_w: Vec<u16> = OsStr::new(PIPE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 헬퍼가 막 재시작 중이거나 이전 클라이언트를 처리 중일 수 있으므로 named pipe의
    // 표준 대기 방식(WaitNamedPipeW)으로 짧게 재시도한다.
    unsafe {
        winapi::um::namedpipeapi::WaitNamedPipeW(path_w.as_ptr(), 200);
    }

    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(5) => Err(NativeError::HelperUnauthorized), // ERROR_ACCESS_DENIED
            _ => Err(NativeError::HelperUnavailable),
        };
    }

    Ok(handle)
}

/// 프로세스 이름으로 PID 찾기 (attach 시점의 1회성 존재 확인용)
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
