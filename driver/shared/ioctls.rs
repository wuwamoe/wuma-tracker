pub const DEVICE_NAME: &str = r"\\.\WumaDisplayService";
pub const TARGET_PROCESS_NAME: &str = "Client-Win64-Shipping.exe";

const fn ctl(function: u32) -> u32 {
    (0x22 << 16) | (function << 2)
}

pub const IOCTL_GET_LOCATION: u32 = ctl(0x802);

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
}
