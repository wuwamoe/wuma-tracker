use std::time::{SystemTime, UNIX_EPOCH};
use ::rand::Rng;

// 8자리 Base36 코드 생성
const CODE_LENGTH: usize = 8;

// 공간 분할 정의: 5자리(시간) + 3자리(랜덤)
const TIMESTAMP_CHARS: u32 = 5;
const RANDOM_CHARS: u32 = 3;

// 각 공간의 크기를 계산 (36^5, 36^3)
const TIMESTAMP_MODULO: u64 = 36u64.pow(TIMESTAMP_CHARS);
const RANDOM_MODULO: u64 = 36u64.pow(RANDOM_CHARS);

// Base36 인코딩에 사용할 문자셋 (0-9, A-Z)
const BASE36_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/**
 * u64 숫자를 지정된 길이의 Base36 문자열로 변환하는 헬퍼 함수.
 * 변환 후 길이에 맞게 앞부분을 '0'으로 채웁니다.
 */
fn to_base36_string(mut value: u64, length: usize) -> String {
    // 0일 경우, 지정된 길이만큼 "0"을 채워서 반환
    if value == 0 {
        return "0".repeat(length);
    }

    let mut code = String::with_capacity(length);
    while value > 0 {
        let index = (value % 36) as usize;
        code.push(BASE36_CHARS[index] as char);
        value /= 36;
    }

    // 생성된 문자는 낮은 자리수부터이므로 뒤집어줍니다.
    let mut reversed_code: String = code.chars().rev().collect();

    // 최종 길이가 `CODE_LENGTH`가 되도록 앞부분을 '0'으로 채웁니다.
    while reversed_code.len() < length {
        reversed_code.insert(0, '0');
    }

    reversed_code
}

/**
 * Base36을 사용하여 8자리의 URL-safe 룸 코드를 생성합니다.
 * 시간 단위는 10ms로 설정되어 있습니다.
 */
pub fn generate_room_code_base36() -> String {
    // 1. 타임스탬프 부분 생성
    let now = SystemTime::now();
    let millis_since_epoch = now
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards, system clock is unreliable.")
        .as_millis() as u64;

    // --- 시간 단위를 10ms로 설정 ---
    let time_interval = millis_since_epoch / 10;

    // 시간 값이 할당된 공간을 넘지 않도록 나머지 연산(modulo) 처리
    let time_part = time_interval % TIMESTAMP_MODULO;

    // 2. 랜덤 부분 생성
    let mut rng = ::rand::thread_rng();
    // 0부터 (RANDOM_MODULO - 1) 까지의 무작위 숫자 생성
    let random_part = rng.gen_range(0..RANDOM_MODULO);

    // 3. 타임스탬프와 랜덤 부분을 산술적으로 결합
    let combined_value: u64 = time_part * RANDOM_MODULO + random_part;

    // 4. 결합된 숫자를 8자리의 Base36 문자로 인코딩
    to_base36_string(combined_value, CODE_LENGTH)
}