/// 커널 드라이버 기반 프로세스 백엔드
///
/// `ProcessBackend`의 유일한 액션인 `poll()`을 드라이버 IOCTL 1회 요청으로 구현한다.
/// 이 백엔드가 Windows의 유일한 구현이다(native_collector.rs에서
/// `use crate::win_proc_driver::WinProcDriver as PlatformProc;`).
/// 유저모드에서 직접 `ReadProcessMemory`로 스캔하던 예전 구현(`win_proc.rs`)은
/// 삭제됐다 — 드라이버가 이미 이름으로 대상 프로세스를 찾아 하드코딩된 체인을
/// 읽는 것까지 전부 처리하므로 되돌아갈 이유가 없다.
///
/// 이 앱은 `\\.\WumaDisplayService`를 직접 열고 `IOCTL_GET_LOCATION`을 직접 호출한다
/// (더 이상 헬퍼 서비스/named pipe를 거치지 않음 — 앱이 이미 관리자 권한으로만
/// 실행되므로, 별도 프로세스가 핸들을 대신 들고 있어야 할 이유가 없다는 판단.
/// `driver/docs/DIRECT-ACCESS-PLAN.md` 참고). 요청/응답 온-와이어 레이아웃은
/// `driver/shared/ioctls.rs`가 원본(source of truth)이다.
///
/// 프로세스 attach 자체도 드라이버가 매 호출마다 내부적으로 처리하므로 (이름으로
/// 프로세스를 찾아 붙는 것까지 IOCTL 한 번에 포함), 이 백엔드는 별도의 "붙어있는
/// 핸들"을 들고 있지 않는다 — `poll()`만 반복 호출하면 된다. 드라이버 디바이스 핸들
/// 자체는 매 poll마다 새로 열고 닫는다: 드라이버가 단일 인스턴스 핸들만 허용하므로
/// (동시에 하나의 열린 핸들만 허용), 핸들을 오래 들고 있을 이유가 없고, 오히려 앱이
/// 재시작되거나 여러 인스턴스가 뜨는 경우를 자연스럽게 처리한다.
use std::mem;
use std::path::PathBuf;

use anyhow::{bail, Result};
use winapi::um::{
    fileapi::{CreateFileW, OPEN_EXISTING},
    handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
    ioapiset::DeviceIoControl,
    winnt::{GENERIC_READ, GENERIC_WRITE},
};

use crate::offsets::{GWorldScanConfig, WuwaOffset};
use crate::process_backend::ProcessBackend;
use crate::types::NativeError;

#[path = "../../driver/shared/ioctls.rs"]
mod ioctls;
use ioctls::{GetLocationResponse, DEVICE_NAME, IOCTL_GET_LOCATION};

const STAGE_DRIVER_UNAVAILABLE: u8 = 0xFF;
const STAGE_TARGET_MISSING: u8 = 0xFE;

pub struct WinProcDriver;

impl WinProcDriver {
    /// WinProc::new와 동일한 시그니처 — native_collector.rs 무수정 교체용.
    /// scan_config는 더 이상 사용되지 않는다: World Anchor/포인터 체인은 드라이버에
    /// 하드코딩되어 있고, 이 앱은 런타임에 그것을 바꿀 방법이 없다.
    pub fn new(name: &str, _cache_dir: PathBuf, _scan_config: Option<GWorldScanConfig>) -> Result<Self> {
        // attach 버튼을 눌렀는데 게임이 아예 안 떠있으면 즉시 실패로 피드백한다.
        // (이후의 좌표 조회는 드라이버가 매번 알아서 프로세스를 찾으므로 이 확인은
        // 여기서 1회, "붙기 시도" 자체의 성공/실패 판정용으로만 쓰인다.)
        if find_pid_by_name(name).is_none() {
            bail!("게임이 실행 중이 아닙니다.");
        }
        Ok(Self)
    }
}

impl ProcessBackend for WinProcDriver {
    /// 오프셋 인자는 참조하지 않는다: 실제 포인터 체인 내용은 드라이버 내부에
    /// 하드코딩되어 있다 (원격 오프셋 파일의 항목 이름/버전과는 무관 — 예전엔
    /// 그 이름이 "v2"로 고정돼 있다고 가정하는 체크가 있었는데, 원격 파일의
    /// 항목 이름이 게임 버전 문자열(예: "v3.4.0+")로 바뀌면서 항상 실패하게
    /// 됐던 죽은 게이트였다. 드라이버 모드에선 이 값 자체가 필요 없으므로 제거).
    fn poll(&mut self, _offsets: &[WuwaOffset]) -> Result<crate::types::PlayerInfo, NativeError> {
        call_driver_get_location()
    }

    fn diagnostics(&self) -> String {
        "driver".to_string()
    }
}

// ── 드라이버 IOCTL ───────────────────────────────────────────────────────────

fn call_driver_get_location() -> Result<crate::types::PlayerInfo, NativeError> {
    let device = open_driver()?;

    let mut resp = GetLocationResponse {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
        stage: 0,
        _pad: [0; 3],
    };
    let mut returned: u32 = 0;

    let ok = unsafe {
        DeviceIoControl(
            device,
            IOCTL_GET_LOCATION,
            std::ptr::null_mut(),
            0,
            &mut resp as *mut _ as *mut _,
            mem::size_of::<GetLocationResponse>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(device) };

    if ok == 0 || returned as usize != mem::size_of::<GetLocationResponse>() {
        return Err(NativeError::DriverUnavailable);
    }

    // 프로세스를 못 찾았다는 것만 "연결 해제"로 취급한다. 그 외(드라이버 불가,
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
/// 8: 최종 좌표(FTransform) 읽기 실패.
fn describe_driver_stage(stage: u8) -> String {
    match stage {
        1 => "메인 모듈 베이스 또는 World Anchor 탐색 실패".to_string(),
        2..=7 => format!("포인터 체인 {}단계 역참조 실패", stage - 1),
        8 => "좌표 트랜스폼(FTransform) 읽기 실패".to_string(),
        other => format!("드라이버 체인 실패 (알 수 없는 stage={})", other),
    }
}

/// `\\.\WumaDisplayService`를 연다. 드라이버가 단일 인스턴스 핸들만 허용하므로,
/// 앱의 다른 인스턴스나 이전 호출의 핸들이 아직 열려 있으면 이 호출은
/// ACCESS_DENIED로 실패한다 — 그 경우도 `DriverUnavailable`로 보고한다(앱 입장에서
/// "드라이버 미로드"와 "이미 다른 핸들이 열려 있음"을 구분해도 사용자가 취할 수
/// 있는 조치가 다르지 않다).
fn open_driver() -> Result<winapi::um::winnt::HANDLE, NativeError> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let path_w: Vec<u16> = OsStr::new(DEVICE_NAME)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

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
        return Err(NativeError::DriverUnavailable);
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
