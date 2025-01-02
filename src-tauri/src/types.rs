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
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
}


#[repr(C)]
#[derive(Copy, Clone, serde::Serialize)]
pub struct FIntVector {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}