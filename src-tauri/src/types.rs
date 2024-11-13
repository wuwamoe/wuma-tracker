#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FVector {
    x: f32,
    y: f32,
    z: f32,
}

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FRotator {
    pitch: f32,
    yaw: f32,
    roll: f32,
}

#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct PlayerInfo {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
}
