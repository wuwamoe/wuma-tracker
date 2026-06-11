use crate::offsets::WuwaOffset;
use crate::types::NativeError::{PointerChainError, ValueReadError};
use crate::types::{FIntVector, FTransformDouble, NativeError, PlayerInfo};
use std::f32::consts::PI;
use std::mem::{self, MaybeUninit};

pub trait ProcessBackend {
    fn is_alive(&self) -> bool;
    fn read_bytes(&self, address: u64, buffer: &mut [u8]) -> Result<(), NativeError>;
    fn read_gworld(&self, offset: &WuwaOffset) -> Result<u64, NativeError>;
    fn rescan_gworld(&mut self) {}

    /// GWorld를 정상적으로 찾은 상태인지 반환한다.
    /// Windows: 초기 스캔 성공 여부. macOS: 항상 true (심볼 테이블 기반).
    fn gworld_ready(&self) -> bool { true }

    fn active_offset_name(&self, offset: &WuwaOffset) -> String {
        offset.name.clone()
    }

    fn read_memory<T: Copy>(&self, address: u64) -> Result<T, NativeError> {
        if address == 0 {
            return Err(PointerChainError {
                message: "원격 메모리 주소가 0입니다.".to_string(),
            });
        }

        unsafe {
            let mut value = MaybeUninit::<T>::uninit();
            let buffer =
                std::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, mem::size_of::<T>());

            self.read_bytes(address, buffer)?;
            Ok(value.assume_init())
        }
    }
}

pub fn select_player_info<B: ProcessBackend>(
    backend: &B,
    cached_offset: &mut Option<WuwaOffset>,
    offsets: &[WuwaOffset],
) -> Result<PlayerInfo, NativeError> {
    if !backend.is_alive() {
        return Err(NativeError::ProcessTerminated);
    }

    if let Some(offset) = cached_offset.as_ref() {
        match read_player_info(backend, offset) {
            Ok(location) => return Ok(location),
            Err(e) => {
                log::warn!(
                    "Cached offset {} failed, retrying all variants: {}",
                    backend.active_offset_name(offset),
                    e
                );
            }
        }
        *cached_offset = None;
    }

    let mut first_err: Option<NativeError> = None;
    for (i, offset) in offsets.iter().enumerate() {
        match read_player_info(backend, offset) {
            Ok(location) => {
                log::info!(
                    "Offset variant #{} ({}) succeeded.",
                    i + 1,
                    backend.active_offset_name(offset)
                );
                *cached_offset = Some(offset.clone());
                return Ok(location);
            }
            Err(e) => {
                log::debug!(
                    "Offset variant #{} ({}) failed: {}",
                    i + 1,
                    backend.active_offset_name(offset),
                    e
                );
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }

    // 모든 variant 실패 시, 첫 variant의 실제 실패 원인을 그대로 노출한다.
    // (실패 단계 = GWorld(.data) read 인지, 이후 포인터 체인인지 + OS 에러 코드 포함)
    Err(first_err.unwrap_or(PointerChainError {
        message: "사용 가능한 버전 값을 찾지 못했습니다.".to_string(),
    }))
}

fn read_player_info<B: ProcessBackend>(
    backend: &B,
    offset: &WuwaOffset,
) -> Result<PlayerInfo, NativeError> {
    let gworld = backend.read_gworld(offset)?;

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
        match backend.read_memory::<u64>(target) {
            Ok(v) => {
                last_addr = v;
            }
            Err(e) => {
                return Err(PointerChainError {
                    message: format!(
                        "포인터 체인 '{}' 단계 실패: 읽기주소={:X} (부모={:X}+오프셋={:X}), 부모포인터상태=[{}] | {}",
                        name,
                        target,
                        last_addr,
                        field_offset,
                        classify_ptr(last_addr),
                        e
                    ),
                });
            }
        }
    }

    let transform_addr = last_addr + offset.uscenecomponent_componenttoworld;
    let location = backend
        .read_memory::<FTransformDouble>(transform_addr)
        .map_err(|e| ValueReadError {
            message: format!(
                "FTransform 위치 ({:X})의 값을 읽지 못했습니다: {}",
                transform_addr, e
            ),
        })?;

    let (roll, pitch, yaw) = quat_to_euler(
        location.rot_x,
        location.rot_y,
        location.rot_z,
        location.rot_w,
    );

    let persistent_level_addr = gworld + offset.uworld_persistentlevel;
    let persistent_level = backend
        .read_memory::<u64>(persistent_level_addr)
        .map_err(|e| PointerChainError {
            message: format!(
                "WorldOrigin을 위한 PersistentLevel 위치 ({:X})의 주소 값을 읽지 못했습니다: {}",
                persistent_level_addr, e
            ),
        })?;

    let world_origin_addr = persistent_level + offset.ulevel_lastworldorigin;
    let root_location = backend
        .read_memory::<FIntVector>(world_origin_addr)
        .map_err(|e| ValueReadError {
            message: format!(
                "LastWorldOrigin 위치 ({:X})의 값을 읽지 못했습니다: {}",
                world_origin_addr, e
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

/// 포인터 값의 타당성을 분류해 실패 원인 진단을 돕는다.
/// - NULL/거의NULL: 게임 상태 문제(아직 월드 미진입, 폰 없음 등) 또는 오프셋이 null 슬롯을 가리킴
/// - 비정상범위: 오프셋/버전 불일치로 엉뚱한 값을 따라감, 또는 ACE의 포인터 암호화/셔플 의심
/// - 정상범위인데 읽기 실패: 페이지 보호/해제 의심
fn classify_ptr(p: u64) -> &'static str {
    const USERMODE_MAX: u64 = 0x0000_7FFF_FFFF_FFFF; // Win x64 유저모드 상한
    if p == 0 {
        "NULL"
    } else if p < 0x1_0000 {
        "거의NULL(작은값)"
    } else if p > USERMODE_MAX {
        "비정상(비canonical/커널영역)"
    } else if p & 0xF != 0 {
        "정상범위(미정렬)"
    } else {
        "정상범위(usermode)"
    }
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
