//! 트래커 ↔ 드라이버 공유 인터페이스 (문서용 참조)
//!
//! 이 파일은 어느 크레이트에도 `mod`/`include!`로 컴파일되어 들어가지 않는다 —
//! driver/src/main.rs(IOCTL 핸들러)와 src-tauri/src/win_proc_driver.rs(호출부)의
//! 실제 온-와이어 레이아웃을 사람이 읽기 쉽게 정리해 둔 참조 문서다.
//! ABI를 바꿀 때는 반드시 두 파일을 함께 수정하고, 이 문서도 갱신한다.

/// 디바이스 이름 (드라이버가 생성하는 심볼릭 링크)
pub const DEVICE_NAME: &str = r"\\.\WumaDisplayService";

/// IOCTL 코드 정의
/// CTL_CODE(DeviceType=0x22, Function, Method=0(BUFFERED), Access=0)
/// = (0x22 << 16) | (Function << 2)
/// Function 범위 0x800~ 는 서드파티 전용
pub const IOCTL_AUTH: u32 = 0x22_2000; // 0x800 << 2, 세션 인증
pub const IOCTL_SET_CONFIG: u32 = 0x22_2004; // 0x801 << 2, 포인터 체인/오프셋 설정
pub const IOCTL_GET_LOCATION: u32 = 0x22_2008; // 0x802 << 2, 좌표 조회 (SET_CONFIG 필요)
pub const IOCTL_PATTERN_SEARCH: u32 = 0x22_2010; // 0x804 << 2, .pdata 패턴 스캔 (GWorld RVA)

// ── 요청/응답 구조체 ──────────────────────────────────────────────────────────
// 모두 repr(C)로 ABI 고정. 필드 순서/크기를 바꾸면 양쪽 크레이트가 동시에 깨진다.

/// IOCTL_AUTH 요청 (8 bytes)
#[repr(C)]
pub struct AuthRequest {
    /// 트래커 자신의 PID (드라이버가 호출자 PID와 비교 검증)
    pub pid: u32,
    pub _pad: u32,
}

/// IOCTL_AUTH 응답 (8 bytes)
#[repr(C)]
pub struct AuthResponse {
    /// 발급된 세션 토큰 (이후 모든 IOCTL에 포함)
    pub token: u64,
}

/// IOCTL_SET_CONFIG 요청 (가변 길이, 최소 48 + (chain_len+origin_chain_len)*8 bytes)
///
/// 레이아웃:
///   [0..8]   token
///   [8..12]  pid (u32)
///   [12]     chain_len (u8, max 16)
///   [13]     origin_chain_len (u8, max 16)
///   [14..16] padding
///   [16..24] base_addr
///   [24..32] anchor_rva      (보통 GWorld RVA)
///   [32..40] transform_offset (chain 끝에서 FTransform까지 오프셋)
///   [40..48] origin_offset    (origin_chain 끝에서 FIntVector까지 오프셋)
///   [48..]   chain[chain_len * 8] ++ origin_chain[origin_chain_len * 8]
#[repr(C)]
pub struct SetConfigRequest {
    pub token: u64,
    pub pid: u32,
    pub chain_len: u8,
    pub origin_chain_len: u8,
    pub _pad: u16,
    pub base_addr: u64,
    pub anchor_rva: u64,
    pub transform_offset: u64,
    pub origin_offset: u64,
    // 이어서 chain[chain_len], origin_chain[origin_chain_len] (u64 배열, 가변 길이)
}

/// IOCTL_GET_LOCATION 요청 (8 bytes)
#[repr(C)]
pub struct GetLocationRequest {
    pub token: u64,
}

/// IOCTL_GET_LOCATION 응답 (28 bytes)
#[repr(C)]
pub struct GetLocationResponse {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    /// 0=성공, 1~=실패한 체인 단계 (진단용)
    pub stage: u8,
    pub _pad: [u8; 3],
}

/// IOCTL_PATTERN_SEARCH 요청 (80 bytes)
#[repr(C)]
pub struct PatternSearchRequest {
    pub token: u64,
    pub target_pid: u32,
    pub _pad: u32,
    /// 스캔 시작 주소 (보통 base_address)
    pub base_address: u64,
    /// 0이면 드라이버가 PE 헤더에서 직접 SizeOfImage를 읽는다
    pub image_size: u32,
    pub prefix_len: u8,
    pub suffix_len: u8,
    pub _pad2: u16,
    /// prefix 바이트 (최대 16)
    pub prefix: [u8; 16],
    /// suffix 바이트 (최대 32, 0xFF = wildcard)
    pub suffix: [u8; 32],
}

/// IOCTL_PATTERN_SEARCH 응답 (8 bytes)
#[repr(C)]
pub struct PatternSearchResponse {
    /// 찾은 GWorld RVA (0이면 실패)
    pub gworld_rva: u64,
}
