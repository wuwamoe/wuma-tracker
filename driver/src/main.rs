//! WumaTracker Player Position Service
//!
//! Single-purpose kernel driver for WumaTracker player world coordinates.
//! No write path, no external scan request path, no generic memory access path,
//! and no runtime configuration surface.

#![no_std]
#![no_main]
#![allow(non_snake_case, non_camel_case_types)]

extern crate alloc;

use wdk_sys::{
    ntddk::{
        ExAllocatePool2, ExFreePoolWithTag, IoCreateDevice, IoCreateSymbolicLink, IoDeleteDevice,
        IoDeleteSymbolicLink, IofCompleteRequest, ObDereferenceObjectDeferDelete,
    },
    DO_BUFFERED_IO, DO_DEVICE_INITIALIZING, FILE_DEVICE_UNKNOWN, NTSTATUS, PDEVICE_OBJECT,
    PEPROCESS, PIRP, PUNICODE_STRING, PVOID, SIZE_T, ULONG, ULONG64, UNICODE_STRING,
};

#[used]
#[link_section = ".rdata"]
static WUMATRACKER_PURPOSE: &str = "WumaTracker Player Position Service; reads only player world coordinates for the legitimate WumaTracker overlay service; no write, no generic pattern scan, no generic memory access, no runtime configuration.";

const STATUS_SUCCESS: NTSTATUS = 0;
const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023u32 as i32;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;
const STATUS_NOT_FOUND: NTSTATUS = 0xC000_0225u32 as i32;

const POOL_FLAG_NON_PAGED: ULONG64 = 0x40;
const MM_COPY_MEMORY_VIRTUAL: ULONG = 0x1;
const POOL_TAG: ULONG = u32::from_le_bytes(*b"WuDr");
const ALLOC_TAG: u32 = u32::from_le_bytes(*b"WuAl");

const IRP_IO_STATUS: usize = 0x30;
const IRP_IO_INFORMATION: usize = 0x38;
const IRP_SYSTEM_BUFFER: usize = 0x70;
const IRP_CURRENT_STACK: usize = 0xB8;
const STACK_IOCTL_CODE: usize = 0x08;
const STACK_IN_LEN: usize = 0x10;
const STACK_OUT_LEN: usize = 0x18;

const DRV_DEVICE_OBJECT: usize = 0x08;
const DRV_UNLOAD: usize = 0x68;
const DRV_MAJOR_FUNCTION: usize = 0x70;

const fn ctl(func: u32) -> u32 {
    (0x22 << 16) | (func << 2)
}
const IOCTL_GET_LOCATION: u32 = ctl(0x802);

const USER_MIN: u64 = 0x1_0000;
const USER_MAX: u64 = 0x7FFF_FFFF_FFFF;
const PEB_IMAGE_BASE_ADDRESS: u64 = 0x10;
const GWORLD_PATTERN_PREFIX: &[u8] = &[0x48, 0x8B, 0x1D];
const GWORLD_PATTERN_SUFFIX: &[u8] = &[0x48, 0x85, 0xDB, 0x74, 0xFF, 0x41, 0xB0, 0x01];
const TARGET_PROCESS_NAME_UTF16: [u16; 25] = [
    0x43, 0x6C, 0x69, 0x65, 0x6E, 0x74, 0x2D, 0x57, 0x69, 0x6E, 0x36, 0x34, 0x2D, 0x53, 0x68, 0x69,
    0x70, 0x70, 0x69, 0x6E, 0x67, 0x2E, 0x65, 0x78, 0x65,
];

struct PlayerCoordConfig {
    fallback_gworld_rva: u64,
    chain: [u64; 6],
    transform_offset: u64,
    origin_chain: [u64; 1],
    origin_offset: u64,
}

static PLAYER_COORD_CONFIG: PlayerCoordConfig = PlayerCoordConfig {
    fallback_gworld_rva: 157_596_584,
    chain: [440, 64, 0, 56, 832, 416],
    transform_offset: 480,
    origin_chain: [56],
    origin_offset: 200,
};

#[repr(C)]
struct MmCopyAddr {
    va: PVOID,
}

extern "system" {
    fn MmCopyMemory(
        Dst: PVOID,
        Src: MmCopyAddr,
        Len: SIZE_T,
        Flags: ULONG,
        Out: *mut SIZE_T,
    ) -> NTSTATUS;
    fn PsGetProcessPeb(Process: PEPROCESS) -> *mut u8;
    fn PsGetNextProcess(Process: PEPROCESS) -> PEPROCESS;
    fn SeLocateProcessImageName(
        Process: PEPROCESS,
        ImageFileName: *mut PUNICODE_STRING,
    ) -> NTSTATUS;
    fn ExFreePool(P: PVOID);
    fn KeStackAttachProcess(Process: PEPROCESS, ApcState: *mut u8);
    fn KeUnstackDetachProcess(ApcState: *mut u8);
}

static DEVICE_NT: &[u16] = &[
    0x5C, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x5C, 0x57, 0x75, 0x6D, 0x61, 0x44, 0x69, 0x73, 0x70,
    0x6C, 0x61, 0x79, 0x53, 0x65, 0x72, 0x76, 0x69, 0x63, 0x65, 0,
];
static DEVICE_DOS: &[u16] = &[
    0x5C, 0x5C, 0x2E, 0x5C, 0x57, 0x75, 0x6D, 0x61, 0x44, 0x69, 0x73, 0x70, 0x6C, 0x61, 0x79, 0x53,
    0x65, 0x72, 0x76, 0x69, 0x63, 0x65, 0,
];

#[repr(C)]
struct LocationResponse {
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    roll: f32,
    stage: u8,
    _pad: [u8; 3],
}

#[repr(C)]
struct FTransform {
    rx: f32,
    ry: f32,
    rz: f32,
    rw: f32,
    lx: f32,
    ly: f32,
    lz: f32,
    _scale: [f32; 3],
}

#[repr(C)]
struct FIntVector {
    x: i32,
    y: i32,
    z: i32,
}

#[inline]
fn is_valid_ptr(addr: u64) -> bool {
    addr >= USER_MIN && addr <= USER_MAX
}

unsafe fn ustr(buf: &[u16]) -> UNICODE_STRING {
    let len = ((buf.len() - 1) * 2) as u16;
    UNICODE_STRING {
        Length: len,
        MaximumLength: len + 2,
        Buffer: buf.as_ptr() as *mut u16,
    }
}

#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(drv: *mut u8, _: PUNICODE_STRING) -> NTSTATUS {
    let mut nt = ustr(DEVICE_NT);
    let mut dos = ustr(DEVICE_DOS);
    let mut dev: PDEVICE_OBJECT = core::ptr::null_mut();

    let s = IoCreateDevice(
        drv as *mut _,
        0,
        &mut nt,
        FILE_DEVICE_UNKNOWN,
        0,
        0,
        &mut dev,
    );
    if s != STATUS_SUCCESS {
        return s;
    }
    let s = IoCreateSymbolicLink(&mut dos, &mut nt);
    if s != STATUS_SUCCESS {
        IoDeleteDevice(dev);
        return s;
    }

    let flags = (dev as *mut u8).add(0x1C) as *mut u32;
    *flags = (*flags | DO_BUFFERED_IO) & !DO_DEVICE_INITIALIZING;

    let mf = drv.add(DRV_MAJOR_FUNCTION) as *mut usize;
    *mf.add(0) = dispatch_ok as usize;
    *mf.add(2) = dispatch_ok as usize;
    *mf.add(14) = dispatch_ioctl as usize;
    *(drv.add(DRV_UNLOAD) as *mut usize) = unload as usize;

    STATUS_SUCCESS
}

unsafe extern "system" fn unload(drv: *mut u8) {
    let mut dos = ustr(DEVICE_DOS);
    IoDeleteSymbolicLink(&mut dos);
    let dev = *(drv.add(DRV_DEVICE_OBJECT) as *mut PDEVICE_OBJECT);
    if !dev.is_null() {
        IoDeleteDevice(dev);
    }
}

unsafe extern "system" fn dispatch_ok(_: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    complete(irp, STATUS_SUCCESS, 0)
}

unsafe extern "system" fn dispatch_ioctl(_: PDEVICE_OBJECT, irp: PIRP) -> NTSTATUS {
    let b = irp as *mut u8;
    let buf = *(b.add(IRP_SYSTEM_BUFFER) as *mut *mut u8);
    let stack = *(b.add(IRP_CURRENT_STACK) as *mut *mut u8);
    let code = *(stack.add(STACK_IOCTL_CODE) as *const u32);
    let ilen = *(stack.add(STACK_IN_LEN) as *const u32) as usize;
    let olen = *(stack.add(STACK_OUT_LEN) as *const u32) as usize;

    let (s, n) = match code {
        IOCTL_GET_LOCATION => on_get_location(buf, ilen, olen),
        _ => (STATUS_INVALID_PARAMETER, 0),
    };
    complete(irp, s, n)
}

unsafe fn complete(irp: PIRP, status: NTSTATUS, info: usize) -> NTSTATUS {
    let b = irp as *mut u8;
    *(b.add(IRP_IO_STATUS) as *mut i32) = status;
    *(b.add(IRP_IO_INFORMATION) as *mut usize) = info;
    IofCompleteRequest(irp, 0);
    status
}

unsafe fn kread(proc: PEPROCESS, addr: u64, out: *mut u8, len: usize) -> bool {
    let mut apc = [0u8; 72];
    KeStackAttachProcess(proc, apc.as_mut_ptr());
    let mut copied: SIZE_T = 0;
    let s = MmCopyMemory(
        out as PVOID,
        MmCopyAddr { va: addr as PVOID },
        len as SIZE_T,
        MM_COPY_MEMORY_VIRTUAL,
        &mut copied,
    );
    KeUnstackDetachProcess(apc.as_mut_ptr());
    s == STATUS_SUCCESS && copied == len as SIZE_T
}

unsafe fn kread_ptr(proc: PEPROCESS, addr: u64) -> Option<u64> {
    if !is_valid_ptr(addr) {
        return None;
    }
    let mut v = 0u64;
    if kread(proc, addr, &mut v as *mut _ as *mut u8, 8) && is_valid_ptr(v) {
        Some(v)
    } else {
        None
    }
}

unsafe fn on_get_location(buf: *mut u8, ilen: usize, olen: usize) -> (NTSTATUS, usize) {
    if ilen != 0 {
        return (STATUS_INVALID_PARAMETER, 0);
    }
    if olen < core::mem::size_of::<LocationResponse>() || buf.is_null() {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }

    let proc = match find_target_process() {
        Some(proc) => proc,
        None => return (STATUS_NOT_FOUND, 0),
    };

    let result = read_player_world_position(proc);
    ObDereferenceObjectDeferDelete(proc as PVOID);

    let r = &mut *(buf as *mut LocationResponse);
    *r = LocationResponse {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
        stage: 0,
        _pad: [0; 3],
    };

    match result {
        Ok((x, y, z, p, yw, rl)) => {
            r.x = x;
            r.y = y;
            r.z = z;
            r.pitch = p;
            r.yaw = yw;
            r.roll = rl;
            (STATUS_SUCCESS, core::mem::size_of::<LocationResponse>())
        }
        Err(stage) => {
            r.stage = stage;
            (
                STATUS_UNSUCCESSFUL,
                core::mem::size_of::<LocationResponse>(),
            )
        }
    }
}

unsafe fn find_target_process() -> Option<PEPROCESS> {
    let mut cursor: PEPROCESS = core::ptr::null_mut();
    loop {
        let next = PsGetNextProcess(cursor);
        if !cursor.is_null() {
            ObDereferenceObjectDeferDelete(cursor as PVOID);
        }
        if next.is_null() {
            return None;
        }
        if process_image_name_matches(next) {
            return Some(next);
        }
        cursor = next;
    }
}

unsafe fn process_image_name_matches(proc: PEPROCESS) -> bool {
    let mut image_name: PUNICODE_STRING = core::ptr::null_mut();
    let status = SeLocateProcessImageName(proc, &mut image_name);
    if image_name.is_null() {
        return false;
    }

    let matched = status == STATUS_SUCCESS && unicode_path_ends_with_target(&*image_name);
    ExFreePool(image_name as PVOID);
    matched
}

unsafe fn unicode_path_ends_with_target(name: &UNICODE_STRING) -> bool {
    let len = (name.Length / 2) as usize;
    if name.Buffer.is_null() || len < TARGET_PROCESS_NAME_UTF16.len() {
        return false;
    }
    let chars = core::slice::from_raw_parts(name.Buffer, len);
    let start = len - TARGET_PROCESS_NAME_UTF16.len();
    for i in 0..TARGET_PROCESS_NAME_UTF16.len() {
        if to_ascii_lower_u16(chars[start + i]) != to_ascii_lower_u16(TARGET_PROCESS_NAME_UTF16[i])
        {
            return false;
        }
    }
    true
}

const fn to_ascii_lower_u16(ch: u16) -> u16 {
    if ch >= 0x41 && ch <= 0x5A {
        ch + 0x20
    } else {
        ch
    }
}

unsafe fn read_player_world_position(
    proc: PEPROCESS,
) -> Result<(f32, f32, f32, f32, f32, f32), u8> {
    let base = read_main_module_base(proc).ok_or(1u8)?;
    let anchor_rva =
        find_hardcoded_gworld_anchor(proc, base).unwrap_or(PLAYER_COORD_CONFIG.fallback_gworld_rva);
    let anchor = kread_ptr(proc, base + anchor_rva).ok_or(1u8)?;

    let mut ptr = anchor;
    for i in 0..PLAYER_COORD_CONFIG.chain.len() {
        ptr = kread_ptr(proc, ptr + PLAYER_COORD_CONFIG.chain[i]).ok_or((i + 2) as u8)?;
    }

    let mut ft = core::mem::MaybeUninit::<FTransform>::uninit();
    if !kread(
        proc,
        ptr + PLAYER_COORD_CONFIG.transform_offset,
        ft.as_mut_ptr() as *mut u8,
        core::mem::size_of::<FTransform>(),
    ) {
        return Err((PLAYER_COORD_CONFIG.chain.len() + 2) as u8);
    }
    let ft = ft.assume_init();

    let mut optr = anchor;
    for i in 0..PLAYER_COORD_CONFIG.origin_chain.len() {
        optr = kread_ptr(proc, optr + PLAYER_COORD_CONFIG.origin_chain[i])
            .ok_or((PLAYER_COORD_CONFIG.chain.len() + 3 + i) as u8)?;
    }
    let mut iv = core::mem::MaybeUninit::<FIntVector>::uninit();
    if !kread(
        proc,
        optr + PLAYER_COORD_CONFIG.origin_offset,
        iv.as_mut_ptr() as *mut u8,
        core::mem::size_of::<FIntVector>(),
    ) {
        return Err(
            (PLAYER_COORD_CONFIG.chain.len() + PLAYER_COORD_CONFIG.origin_chain.len() + 3) as u8,
        );
    }
    let iv = iv.assume_init();

    let (roll, pitch, yaw) = quat_to_euler(ft.rx, ft.ry, ft.rz, ft.rw);
    Ok((
        ft.lx + iv.x as f32,
        ft.ly + iv.y as f32,
        ft.lz + iv.z as f32,
        pitch,
        yaw,
        roll,
    ))
}

unsafe fn read_main_module_base(proc: PEPROCESS) -> Option<u64> {
    let peb = PsGetProcessPeb(proc);
    if peb.is_null() {
        return None;
    }
    kread_ptr(proc, peb as u64 + PEB_IMAGE_BASE_ADDRESS)
}

fn quat_to_euler(x: f32, y: f32, z: f32, w: f32) -> (f32, f32, f32) {
    use core::f32::consts::PI;
    let roll = libm::atan2f(2.0 * (w * x + y * z), 1.0 - 2.0 * (x * x + y * y));
    let sinp = 2.0 * (w * y - z * x);
    let pitch = if libm::fabsf(sinp) >= 1.0 {
        libm::copysignf(PI / 2.0, sinp)
    } else {
        libm::asinf(sinp)
    };
    let yaw = libm::atan2f(2.0 * (w * z + x * y), 1.0 - 2.0 * (y * y + z * z));
    (roll * 180.0 / PI, pitch * 180.0 / PI, yaw * 180.0 / PI)
}

unsafe fn find_hardcoded_gworld_anchor(proc: PEPROCESS, base: u64) -> Option<u64> {
    const BATCH_GAP: u64 = 4096;
    const HEADER_READ: usize = 4096;
    const MAX_BATCH: usize = 512 * 1024;

    let hb = ExAllocatePool2(POOL_FLAG_NON_PAGED, HEADER_READ as SIZE_T, POOL_TAG) as *mut u8;
    if hb.is_null() {
        return None;
    }

    let header_info = if kread(proc, base, hb, HEADER_READ) {
        let hdr = core::slice::from_raw_parts(hb, HEADER_READ);
        match read_u32(hdr, 0x3C) {
            Some(elf_raw) => {
                let elf = elf_raw as usize;
                if elf + 0x110 <= hdr.len() {
                    let dd = elf + 24 + 112 + 24;
                    match (
                        read_u32(hdr, elf + 0x50),
                        read_u32(hdr, dd),
                        read_u32(hdr, dd + 4),
                    ) {
                        (Some(soi), Some(prva), Some(psz)) => {
                            Some((soi as usize, prva as u64, psz as usize))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            }
            None => None,
        }
    } else {
        None
    };
    ExFreePoolWithTag(hb as PVOID, POOL_TAG);

    let (soi, prva, psz) = header_info?;
    if prva == 0 || psz < 12 {
        return None;
    }

    let pread = psz.min(MAX_BATCH);
    let pb = ExAllocatePool2(POOL_FLAG_NON_PAGED, pread as SIZE_T, POOL_TAG) as *mut u8;
    if pb.is_null() {
        return None;
    }
    if !kread(proc, base + prva, pb, pread) {
        ExFreePoolWithTag(pb as PVOID, POOL_TAG);
        return None;
    }

    let entries = pread / 12;
    let fb =
        ExAllocatePool2(POOL_FLAG_NON_PAGED, (entries * 8) as SIZE_T, POOL_TAG) as *mut (u32, u32);
    if fb.is_null() {
        ExFreePoolWithTag(pb as PVOID, POOL_TAG);
        return None;
    }

    let ps = core::slice::from_raw_parts(pb, pread);
    let fs = core::slice::from_raw_parts_mut(fb, entries);
    let mut n = 0;
    for i in 0..entries {
        let Some(begin) = read_u32(ps, i * 12) else {
            continue;
        };
        let Some(end) = read_u32(ps, i * 12 + 4) else {
            continue;
        };
        if begin > 0 && end > begin && (end as usize) <= soi {
            fs[n] = (begin, end);
            n += 1;
        }
    }
    ExFreePoolWithTag(pb as PVOID, POOL_TAG);
    fs[..n].sort_unstable_by_key(|&(begin, _)| begin);

    let sb = ExAllocatePool2(POOL_FLAG_NON_PAGED, MAX_BATCH as SIZE_T, POOL_TAG) as *mut u8;
    if sb.is_null() {
        ExFreePoolWithTag(fb as PVOID, POOL_TAG);
        return None;
    }

    let instruction_len = GWORLD_PATTERN_PREFIX.len() + 4;
    let pattern_len = instruction_len + GWORLD_PATTERN_SUFFIX.len();
    let mut result = None;
    let mut i = 0;

    'out: while i < n {
        let batch_start = fs[i].0 as u64;
        let mut batch_end = fs[i].1 as u64;
        let mut j = i + 1;
        while j < n {
            if fs[j].0 as u64 - batch_end > BATCH_GAP
                || fs[j].1 as u64 - batch_start > MAX_BATCH as u64
            {
                break;
            }
            batch_end = fs[j].1 as u64;
            j += 1;
        }

        let read_size = ((batch_end - batch_start) as usize).min(MAX_BATCH);
        if read_size >= pattern_len && kread(proc, base + batch_start, sb, read_size) {
            let scan = core::slice::from_raw_parts(sb, read_size);
            'scan: for off in 0..read_size - pattern_len + 1 {
                if scan[off..off + GWORLD_PATTERN_PREFIX.len()] != *GWORLD_PATTERN_PREFIX {
                    continue;
                }
                for (k, &v) in GWORLD_PATTERN_SUFFIX.iter().enumerate() {
                    if v != 0xFF && scan[off + instruction_len + k] != v {
                        continue 'scan;
                    }
                }

                let Some(disp) = read_i32(scan, off + GWORLD_PATTERN_PREFIX.len()) else {
                    continue;
                };
                let instr_rva = batch_start + off as u64;
                let gworld_rva = ((instr_rva as i64) + instruction_len as i64 + disp as i64) as u64;
                if gworld_rva > 0 && gworld_rva < soi as u64 {
                    result = Some(gworld_rva);
                    break 'out;
                }
            }
        }
        i = j;
    }

    ExFreePoolWithTag(sb as PVOID, POOL_TAG);
    ExFreePoolWithTag(fb as PVOID, POOL_TAG);
    result
}

fn read_u32(buf: &[u8], offset: usize) -> Option<u32> {
    let bytes = buf.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_i32(buf: &[u8], offset: usize) -> Option<i32> {
    let bytes = buf.get(offset..offset + 4)?;
    Some(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

use core::alloc::{GlobalAlloc, Layout};

struct KAlloc;
unsafe impl GlobalAlloc for KAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ExAllocatePool2(POOL_FLAG_NON_PAGED, layout.size() as SIZE_T, ALLOC_TAG) as *mut u8
    }
    unsafe fn dealloc(&self, ptr: *mut u8, _: Layout) {
        ExFreePoolWithTag(ptr as PVOID, ALLOC_TAG);
    }
}
#[global_allocator]
static A: KAlloc = KAlloc;

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
