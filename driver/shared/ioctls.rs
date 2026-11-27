pub const DEVICE_NAME: &str = r"\\.\WumaDisplayService";
pub const DEVICE_SYMBOLIC_LINK_NAME: &str = r"\DosDevices\WumaDisplayService";
pub const TARGET_PROCESS_NAME: &str = "Client-Win64-Shipping.exe";
// Matches `mainBinaryName` in src-tauri/tauri.conf.json. Checked at
// IRP_MJ_CREATE as friction against incidental misuse only — not an identity
// guarantee, see driver/docs/DIRECT-ACCESS-PLAN.md.
pub const APP_PROCESS_NAME: &str = "wuma-tracker.exe";

const fn ctl(function: u32) -> u32 {
    (0x22 << 16) | (function << 2)
}

pub const IOCTL_GET_LOCATION: u32 = ctl(0x802);

/// Size of `GetLocationResponse::world_name`.
pub const WORLD_NAME_LEN: usize = 64;

#[repr(C)]
pub struct GetLocationRequest;

#[repr(C)]
pub struct GetLocationResponse {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub pitch: f32,
    pub yaw: f32,
    pub roll: f32,
    pub stage: u8,
    pub _pad: [u8; 3],
    /// Short name of the loaded world, e.g. `AkiWorld_WP`: the last segment of
    /// the game's map path. NUL-padded ASCII, at most `WORLD_NAME_LEN - 1`
    /// characters. All zero when it couldn't be read; the coordinates are
    /// still valid then.
    pub world_name: [u8; WORLD_NAME_LEN],
}
