use crate::offsets::WuwaOffset;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::types::{FIntVector, FTransformDouble, NativeError, PlayerInfo};
use std::f32::consts::PI;

/// 프로세스 백엔드가 노출해야 하는 유일한 액션.
///
/// "프로세스에 붙기"와 "좌표 읽기"를 분리하지 않는다 — attach는 백엔드 내부에서
/// 알아서 처리하고(이미 붙어 있으면 재사용, 아니면 다시 찾는 식), 외부(native_collector)는
/// 반환된 에러의 종류만으로 상태를 판단한다:
/// - `NativeError::ProcessTerminated`: 대상 프로세스 자체가 없음 → 연결 해제로 취급.
/// - 그 외 모든 에러: 프로세스는 여전히 붙어 있는 것으로 간주하는 일시적 오류.
pub trait ProcessBackend {
    fn poll(&mut self, offsets: &[WuwaOffset]) -> Result<PlayerInfo, NativeError>;

    /// UI에 표시할 진단 라벨 (어떤 오프셋/방식으로 값을 찾았는지)
    fn diagnostics(&self) -> String;
}

/// GWorld로부터 시작하는 UE 포인터 체인을 순회해 PlayerInfo를 얻는다.
/// 사용자 공간에서 직접 메모리를 읽는 백엔드(WinProc, MacProc)가 공용으로 사용한다.
pub fn walk_pointer_chain(
    mut read_u64: impl FnMut(u64) -> Result<u64, NativeError>,
    mut read_transform: impl FnMut(u64) -> Result<FTransformDouble, NativeError>,
    mut read_ivec: impl FnMut(u64) -> Result<FIntVector, NativeError>,
    gworld: u64,
    offset: &WuwaOffset,
) -> Result<PlayerInfo, NativeError> {
    let targets = [
        ("OwningGameInstance", offset.uworld_owninggameinstance),
        ("TArray<*LocalPlayers>", offset.ugameinstance_localplayers),
        ("LocalPlayer", 0),
        ("PlayerController", offset.uplayer_playercontroller),
        ("APawn", offset.aplayercontroller_acknowlegedpawn),
        ("RootComponent", offset.aactor_rootcomponent),
    ];

    let mut last_addr = gworld;
    for (name, field_offset) in targets {
        let target = last_addr + field_offset;
        match read_u64(target) {
            Ok(v) => {
                last_addr = v;
            }
            Err(e) => {
                return Err(PointerChainError {
                    message: format!(
                        "chain['{}' {:X}+{:X} {}]: {}",
                        name, last_addr, field_offset, classify_ptr(last_addr), e
                    ),
                });
            }
        }
    }

    let transform_addr = last_addr + offset.uscenecomponent_componenttoworld;
    let location = read_transform(transform_addr).map_err(|e| ValueReadError {
        message: format!("ftrans@{:X}: {}", transform_addr, e),
    })?;

    let (roll, pitch, yaw) = quat_to_euler(
        location.rot_x,
        location.rot_y,
        location.rot_z,
        location.rot_w,
    );

    let persistent_level_addr = gworld + offset.uworld_persistentlevel;
    let persistent_level = read_u64(persistent_level_addr).map_err(|e| PointerChainError {
        message: format!("plevel@{:X}: {}", persistent_level_addr, e),
    })?;

    let world_origin_addr = persistent_level + offset.ulevel_lastworldorigin;
    let root_location = read_ivec(world_origin_addr).map_err(|e| ValueReadError {
        message: format!("worigin@{:X}: {}", world_origin_addr, e),
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

/// offset 후보들을 순회하며 `walk_pointer_chain`을 시도하고, 성공한 offset을 캐싱한다.
/// 캐싱된 offset이 있으면 그것부터 재시도하고, 실패하면 캐시를 버리고 전체 재순회한다.
pub fn select_and_walk(
    cached_offset: &mut Option<WuwaOffset>,
    offsets: &[WuwaOffset],
    mut read_gworld: impl FnMut(&WuwaOffset) -> Result<u64, NativeError>,
    mut read_u64: impl FnMut(u64) -> Result<u64, NativeError>,
    mut read_transform: impl FnMut(u64) -> Result<FTransformDouble, NativeError>,
    mut read_ivec: impl FnMut(u64) -> Result<FIntVector, NativeError>,
) -> Result<PlayerInfo, NativeError> {
    if let Some(offset) = cached_offset.clone() {
        if let Ok(gworld) = read_gworld(&offset) {
            match walk_pointer_chain(&mut read_u64, &mut read_transform, &mut read_ivec, gworld, &offset) {
                Ok(info) => return Ok(info),
                Err(e) => log::warn!("offset {} miss: {}", offset.name, e),
            }
        }
        *cached_offset = None;
    }

    let mut first_err: Option<NativeError> = None;
    for (i, offset) in offsets.iter().enumerate() {
        let gworld = match read_gworld(offset) {
            Ok(g) => g,
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
                continue;
            }
        };

        match walk_pointer_chain(&mut read_u64, &mut read_transform, &mut read_ivec, gworld, offset) {
            Ok(info) => {
                log::info!("Offset variant #{} ({}) succeeded.", i + 1, offset.name);
                *cached_offset = Some(offset.clone());
                return Ok(info);
            }
            Err(e) => {
                log::debug!("Offset variant #{} ({}) failed: {}", i + 1, offset.name, e);
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }

    Err(first_err.unwrap_or(PointerChainError {
        message: "사용 가능한 버전 값을 찾지 못했습니다.".to_string(),
    }))
}

/// 포인터 값의 타당성을 분류해 실패 원인 진단을 돕는다.
pub fn classify_ptr(p: u64) -> &'static str {
    const USERMODE_MAX: u64 = 0x0000_7FFF_FFFF_FFFF;
    if p == 0 { "NULL" }
    else if p < 0x1_0000 { "~NULL" }
    else if p > USERMODE_MAX { "!canon" }
    else if p & 0xF != 0 { "!align" }
    else { "ok" }
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
