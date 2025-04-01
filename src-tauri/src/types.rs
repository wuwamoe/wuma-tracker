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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalStorageConfig {
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub use_secure_connection: Option<bool>,
}

impl Default for LocalStorageConfig {
    fn default() -> LocalStorageConfig {
        LocalStorageConfig {
            ip: None,
            port: None,
            use_secure_connection: None,
        }
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalState {
    pub proc_state: i32,
    pub server_state: i32,
}

impl Default for GlobalState {
    fn default() -> GlobalState {
        GlobalState {
            proc_state: 0,
            server_state: 0,
        }
    }
}
