//! WumaTracker Kernel Driver
//!
//! Layout:
//!   1. Types / constants / extern declarations
//!   2. Global session state
//!   3. DriverEntry + IOCTL boilerplate
//!   4. Business logic (memory read, pattern scan, location chain)
//!   5. Global allocator + panic handler

#![no_std]
#![no_main]
#![allow(non_snake_case, non_camel_case_types)]

extern crate alloc;

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::SeqCst};
use wdk_sys::{
    ntddk::{
        ExAllocatePool2, ExFreePoolWithTag, IoCreateDevice, IoCreateSymbolicLink, IoDeleteDevice,
        IoDeleteSymbolicLink, IofCompleteRequest, ObDereferenceObjectDeferDelete,
        PsGetCurrentProcessId, PsLookupProcessByProcessId,
    },
    DO_BUFFERED_IO, DO_DEVICE_INITIALIZING, FILE_DEVICE_UNKNOWN, NTSTATUS, PDEVICE_OBJECT,
    PEPROCESS, PIRP, PUNICODE_STRING, PVOID, SIZE_T, ULONG, ULONG64, UNICODE_STRING,
};

// ═══════════════════════════════════════════════════════════════════════════════
// 1. Types / Constants / Extern
// ═══════════════════════════════════════════════════════════════════════════════

// NTSTATUS codes
const STATUS_SUCCESS: NTSTATUS = 0;
const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001u32 as i32;
const STATUS_ACCESS_DENIED: NTSTATUS = 0xC000_0022u32 as i32;
const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023u32 as i32;
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;
const STATUS_NOT_FOUND: NTSTATUS = 0xC000_0225u32 as i32;

// Pool / memory flags
const POOL_FLAG_NON_PAGED: ULONG64 = 0x40;
const MM_COPY_MEMORY_VIRTUAL: ULONG = 0x1;
const POOL_TAG: ULONG = u32::from_le_bytes(*b"WuDr");
const ALLOC_TAG: u32 = u32::from_le_bytes(*b"WuAl");

// IRP struct offsets (x64 Windows 10+)
const IRP_IO_STATUS: usize = 0x30;
const IRP_IO_INFORMATION: usize = 0x38;
const IRP_SYSTEM_BUFFER: usize = 0x70;
const IRP_CURRENT_STACK: usize = 0xB8;
const STACK_IOCTL_CODE: usize = 0x08;
const STACK_IN_LEN: usize = 0x10;
const STACK_OUT_LEN: usize = 0x18;

// DRIVER_OBJECT offsets (x64)
const DRV_DEVICE_OBJECT: usize = 0x08;
const DRV_UNLOAD: usize = 0x68;
const DRV_MAJOR_FUNCTION: usize = 0x70;

// IOCTL codes (METHOD_BUFFERED, FILE_ANY_ACCESS)
const fn ctl(func: u32) -> u32 {
    (0x22 << 16) | (func << 2)
}
const IOCTL_AUTH: u32 = ctl(0x800);
const IOCTL_SET_CONFIG: u32 = ctl(0x801);
const IOCTL_GET_LOCATION: u32 = ctl(0x802);
const IOCTL_PATTERN_SEARCH: u32 = ctl(0x804);

// Pointer validation range (user-mode canonical x64)
const USER_MIN: u64 = 0x1_0000;
const USER_MAX: u64 = 0x7FFF_FFFF_FFFF;

#[inline]
fn is_valid_ptr(addr: u64) -> bool {
    addr >= USER_MIN && addr <= USER_MAX
}

// Extern kernel APIs not exposed by wdk-sys
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
    fn PsGetCurrentProcess() -> PEPROCESS;
    fn KeQuerySystemTimePrecise(Time: *mut i64);
    fn KeStackAttachProcess(Process: PEPROCESS, ApcState: *mut u8);
    fn KeUnstackDetachProcess(ApcState: *mut u8);
    fn PsGetProcessPeb(Process: PEPROCESS) -> *mut u8;
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Global Session State
// ═══════════════════════════════════════════════════════════════════════════════

const MAX_CHAIN: usize = 16;

/// Cached session config — set once via SET_CONFIG, read every GET_LOCATION.
struct SessionConfig {
    pid: u32,
    base_addr: u64,
    anchor_rva: u64,
    /// Pointer chain offsets applied after reading anchor.
    /// Each entry: read ptr at (current + chain[i]), then follow it.
    chain: [u64; MAX_CHAIN],
    chain_len: u8,
    /// After the chain, read FTransform at (final_ptr + transform_offset).
    transform_offset: u64,
    /// Separate chain starting from anchor for world origin.
    origin_chain: [u64; MAX_CHAIN],
    origin_chain_len: u8,
    /// After origin chain, read FIntVector at (final_ptr + origin_offset).
    origin_offset: u64,
}

static SESSION_TOKEN: AtomicU64 = AtomicU64::new(0);
static AUTHED_PID: AtomicU64 = AtomicU64::new(0);
static CONFIG_READY: AtomicU32 = AtomicU32::new(0);

static mut CONFIG: SessionConfig = SessionConfig {
    pid: 0,
    base_addr: 0,
    anchor_rva: 0,
    chain: [0; MAX_CHAIN],
    chain_len: 0,
    transform_offset: 0,
    origin_chain: [0; MAX_CHAIN],
    origin_chain_len: 0,
    origin_offset: 0,
};

#[inline]
fn verify_token(token: u64) -> bool {
    let t = SESSION_TOKEN.load(SeqCst);
    t != 0 && token == t && AUTHED_PID.load(SeqCst) != 0
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. DriverEntry + IOCTL Boilerplate
// ═══════════════════════════════════════════════════════════════════════════════

// Device names (UTF-16LE, null-terminated)
static DEVICE_NT: &[u16] = &[
    0x5C, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x5C, // \Device\backslash
    0x57, 0x75, 0x6D, 0x61, 0x44, 0x69, 0x73, 0x70, 0x6C, 0x61, 0x79, 0x53, 0x65, 0x72, 0x76, 0x69,
    0x63, 0x65, 0,
];
static DEVICE_DOS: &[u16] = &[
    0x5C, 0x5C, 0x2E, 0x5C, // \\.\backslash
    0x57, 0x75, 0x6D, 0x61, 0x44, 0x69, 0x73, 0x70, 0x6C, 0x61, 0x79, 0x53, 0x65, 0x72, 0x76, 0x69,
    0x63, 0x65, 0,
];

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
    // Generate session token from system time
    let mut t = 0i64;
    KeQuerySystemTimePrecise(&mut t);
    SESSION_TOKEN.store((t as u64) ^ 0xDEAD_BEEF_C0DE_1234, SeqCst);

    // Create device object and symbolic link
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

    // Enable buffered I/O, clear initializing flag
    let flags = (dev as *mut u8).add(0x1C) as *mut u32;
    *flags = (*flags | DO_BUFFERED_IO) & !DO_DEVICE_INITIALIZING;

    // Register dispatch functions
    let mf = drv.add(DRV_MAJOR_FUNCTION) as *mut usize;
    *mf.add(0) = dispatch_ok as usize; // IRP_MJ_CREATE
    *mf.add(2) = dispatch_ok as usize; // IRP_MJ_CLOSE
    *mf.add(14) = dispatch_ioctl as usize; // IRP_MJ_DEVICE_CONTROL
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
        IOCTL_AUTH => on_auth(buf, ilen, olen),
        IOCTL_SET_CONFIG => on_set_config(buf, ilen, olen),
        IOCTL_GET_LOCATION => on_get_location(buf, ilen, olen),
        IOCTL_PATTERN_SEARCH => on_pattern_search(buf, ilen, olen),
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

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Business Logic
// ═══════════════════════════════════════════════════════════════════════════════

// ── Kernel memory read primitives ────────────────────────────────────────────

/// Read `len` bytes from target process virtual address.
/// Attaches to the process context, then uses MmCopyMemory.
/// Returns false on any failure — never crashes.
unsafe fn kread(proc: PEPROCESS, addr: u64, out: *mut u8, len: usize) -> bool {
    let mut apc = [0u8; 72]; // KAPC_STATE (x64 size)
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

/// Read a u64 pointer and validate it's in user-mode range.
/// Returns None if read fails or value is outside canonical user space.
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

// ── IOCTL_AUTH ───────────────────────────────────────────────────────────────
// Request:  pid(u32) + pad(u32) = 8 bytes
// Response: token(u64) = 8 bytes

unsafe fn on_auth(buf: *mut u8, ilen: usize, olen: usize) -> (NTSTATUS, usize) {
    if ilen < 8 || olen < 8 {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }
    let caller = PsGetCurrentProcessId() as u64;
    let req_pid = *(buf as *const u32) as u64;
    if req_pid != caller {
        return (STATUS_ACCESS_DENIED, 0);
    }
    AUTHED_PID.store(caller, SeqCst);
    *(buf as *mut u64) = SESSION_TOKEN.load(SeqCst);
    (STATUS_SUCCESS, 8)
}

// ── IOCTL_SET_CONFIG ─────────────────────────────────────────────────────────
// Request layout:
//   [0..8]   token
//   [8..12]  pid (u32)
//   [12]     chain_len (u8, max 16)
//   [13]     origin_chain_len (u8, max 16)
//   [14..16] padding
//   [16..24] base_addr
//   [24..32] anchor_rva
//   [32..40] transform_offset
//   [40..48] origin_offset
//   [48..]   chain[chain_len * 8] ++ origin_chain[origin_chain_len * 8]
// Response: none

unsafe fn on_set_config(buf: *mut u8, ilen: usize, _olen: usize) -> (NTSTATUS, usize) {
    if ilen < 48 {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }
    let token = *(buf as *const u64);
    if !verify_token(token) {
        return (STATUS_ACCESS_DENIED, 0);
    }

    let pid = *(buf.add(8) as *const u32);
    let clen = *buf.add(12) as usize;
    let olen_c = *buf.add(13) as usize;
    let base = *(buf.add(16) as *const u64);
    let gw_rva = *(buf.add(24) as *const u64);
    let t_off = *(buf.add(32) as *const u64);
    let o_off = *(buf.add(40) as *const u64);

    if clen > MAX_CHAIN || olen_c > MAX_CHAIN {
        return (STATUS_INVALID_PARAMETER, 0);
    }
    let needed = 48 + (clen + olen_c) * 8;
    if ilen < needed {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }

    let c = &mut CONFIG;
    c.pid = pid;
    c.base_addr = base;
    c.anchor_rva = gw_rva;
    c.chain_len = clen as u8;
    c.transform_offset = t_off;
    c.origin_chain_len = olen_c as u8;
    c.origin_offset = o_off;

    let chain_ptr = buf.add(48) as *const u64;
    for i in 0..clen {
        c.chain[i] = *chain_ptr.add(i);
    }
    let ochain_ptr = chain_ptr.add(clen);
    for i in 0..olen_c {
        c.origin_chain[i] = *ochain_ptr.add(i);
    }

    CONFIG_READY.store(1, SeqCst);
    (STATUS_SUCCESS, 0)
}

// ── IOCTL_GET_LOCATION ───────────────────────────────────────────────────────
// Request:  token(u64) = 8 bytes
// Response: x,y,z,pitch,yaw,roll (6×f32) + error_stage(u8) + pad(3) = 28 bytes

unsafe fn on_get_location(buf: *mut u8, ilen: usize, olen: usize) -> (NTSTATUS, usize) {
    if ilen < 8 || olen < 28 {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }
    let token = *(buf as *const u64);
    if !verify_token(token) {
        return (STATUS_ACCESS_DENIED, 0);
    }
    if CONFIG_READY.load(SeqCst) == 0 {
        return (STATUS_INVALID_PARAMETER, 0);
    }

    let c = &CONFIG;
    let mut proc: PEPROCESS = core::ptr::null_mut();
    if PsLookupProcessByProcessId(c.pid as u64 as _, &mut proc) != STATUS_SUCCESS {
        return (STATUS_NOT_FOUND, 0);
    }

    let result = walk_chain(proc, c);
    ObDereferenceObjectDeferDelete(proc as PVOID);

    #[repr(C)]
    struct Resp {
        x: f32,
        y: f32,
        z: f32,
        pitch: f32,
        yaw: f32,
        roll: f32,
        stage: u8,
        _p: [u8; 3],
    }
    let r = &mut *(buf as *mut Resp);
    match result {
        Ok((x, y, z, p, yw, rl)) => {
            r.x = x;
            r.y = y;
            r.z = z;
            r.pitch = p;
            r.yaw = yw;
            r.roll = rl;
            r.stage = 0;
            r._p = [0; 3];
            (STATUS_SUCCESS, 28)
        }
        Err(stage) => {
            r.stage = stage;
            r._p = [0; 3];
            (STATUS_UNSUCCESSFUL, 1)
        }
    }
}

// ── Pointer chain traversal (generic) ────────────────────────────────────────

/// Walks the configured pointer chain and reads player coordinates.
///
/// Steps:
///   1. Read anchor pointer from (base_addr + anchor_rva)
///   2. For each entry in chain[]: read ptr at (current + offset), follow it
///   3. Read FTransform (40 bytes) at (final + transform_offset)
///   4. Walk origin_chain[] from anchor, read FIntVector (12 bytes)
///   5. Combine position + world origin, convert quaternion to euler angles
///
/// On failure, returns the 1-based step number where the read failed.
unsafe fn walk_chain(
    proc: PEPROCESS,
    c: &SessionConfig,
) -> Result<(f32, f32, f32, f32, f32, f32), u8> {
    // Step 1: anchor
    let anchor = kread_ptr(proc, c.base_addr + c.anchor_rva).ok_or(1u8)?;

    // Step 2: Follow pointer chain
    let mut ptr = anchor;
    for i in 0..c.chain_len as usize {
        ptr = kread_ptr(proc, ptr + c.chain[i]).ok_or((i + 2) as u8)?;
    }

    // Step 3: Read FTransform at final pointer
    #[repr(C)]
    struct FT {
        rx: f32,
        ry: f32,
        rz: f32,
        rw: f32,
        lx: f32,
        ly: f32,
        lz: f32,
        _s: [f32; 3],
    }
    let mut ft = core::mem::MaybeUninit::<FT>::uninit();
    if !kread(
        proc,
        ptr + c.transform_offset,
        ft.as_mut_ptr() as *mut u8,
        40,
    ) {
        return Err(c.chain_len + 2);
    }
    let ft = ft.assume_init();

    // Step 4: Walk origin chain (starts from anchor)
    let mut optr = anchor;
    for i in 0..c.origin_chain_len as usize {
        optr = kread_ptr(proc, optr + c.origin_chain[i]).ok_or(c.chain_len + 3 + i as u8)?;
    }
    #[repr(C)]
    struct IV {
        x: i32,
        y: i32,
        z: i32,
    }
    let mut iv = core::mem::MaybeUninit::<IV>::uninit();
    if !kread(proc, optr + c.origin_offset, iv.as_mut_ptr() as *mut u8, 12) {
        return Err(c.chain_len + c.origin_chain_len + 3);
    }
    let iv = iv.assume_init();

    // Step 5: Combine and return
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

// ── IOCTL_PATTERN_SEARCH (.pdata batch scan) ─────────────────────────────────
// Request:  token(8) + pid(4) + pad(4) + base(8) + img_size(4)
//           + prefix_len(1) + suffix_len(1) + pad(2)
//           + prefix[16] + suffix[32] = 80 bytes
// Response: anchor_rva(u64) = 8 bytes (0 if not found)

unsafe fn on_pattern_search(buf: *mut u8, ilen: usize, olen: usize) -> (NTSTATUS, usize) {
    if ilen < 80 || olen < 8 {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }
    let token = *(buf as *const u64);
    let pid = *(buf.add(8) as *const u32) as u64;
    let base = *(buf.add(16) as *const u64);
    let img_size = *(buf.add(24) as *const u32) as usize;
    let plen = *buf.add(28) as usize;
    let slen = *buf.add(29) as usize;
    if !verify_token(token) {
        return (STATUS_ACCESS_DENIED, 0);
    }
    if plen == 0 || plen > 16 || slen > 32 {
        return (STATUS_INVALID_PARAMETER, 0);
    }
    let prefix = core::slice::from_raw_parts(buf.add(32), plen);
    let suffix = core::slice::from_raw_parts(buf.add(48), slen);

    let mut proc: PEPROCESS = core::ptr::null_mut();
    if PsLookupProcessByProcessId(pid as _, &mut proc) != STATUS_SUCCESS {
        return (STATUS_NOT_FOUND, 0);
    }
    let rva = scan_anchor(proc, base, img_size, prefix, suffix);
    ObDereferenceObjectDeferDelete(proc as PVOID);
    *(buf as *mut u64) = rva.unwrap_or(0);
    (STATUS_SUCCESS, 8)
}

/// Scan game executable's .pdata section to find anchor RVA via byte pattern.
///
/// How it works:
///   1. Read PE header to locate .pdata (exception directory) — contains
///      begin/end RVA of every function in the exe.
///   2. Sort functions by start address and group adjacent ones into batches
///      (up to 512KB each) to minimize read calls.
///   3. For each batch, read the code and search for a byte pattern:
///      [prefix bytes][4-byte RIP-relative displacement][suffix bytes]
///      where suffix 0xFF means wildcard (match any byte).
///   4. When found, compute: anchor_rva = instruction_RVA + instr_len + disp32
///      and verify it's within image bounds.
///
/// All large buffers are allocated from NonPagedPool to avoid kernel stack overflow.
unsafe fn scan_anchor(
    proc: PEPROCESS,
    base: u64,
    hint: usize,
    prefix: &[u8],
    suffix: &[u8],
) -> Option<u64> {
    const BATCH_GAP: u64 = 4096;
    const MAX_BATCH: usize = 512 * 1024;

    // --- Step 1: Read PE header (4KB on stack — safe) ---
    let mut hdr = [0u8; 4096];
    if !kread(proc, base, hdr.as_mut_ptr(), 4096) {
        return None;
    }

    // Parse e_lfanew to find PE signature offset
    let elf = u32::from_le_bytes(hdr[0x3C..0x40].try_into().ok()?) as usize;
    if elf + 0x110 > 4096 {
        return None;
    }

    // SizeOfImage from optional header
    let soi = if hint > 0 {
        hint
    } else {
        u32::from_le_bytes(hdr[elf + 0x50..elf + 0x54].try_into().ok()?) as usize
    };

    // Exception directory (DataDirectory[3]) — .pdata RVA and size
    let dd = elf + 24 + 112 + 24;
    let prva = u32::from_le_bytes(hdr[dd..dd + 4].try_into().ok()?) as u64;
    let psz = u32::from_le_bytes(hdr[dd + 4..dd + 8].try_into().ok()?) as usize;
    if prva == 0 || psz < 12 {
        return None;
    }

    // --- Step 2: Read .pdata into NonPagedPool ---
    let pread = psz.min(MAX_BATCH);
    let pb = ExAllocatePool2(POOL_FLAG_NON_PAGED, pread as SIZE_T, POOL_TAG) as *mut u8;
    if pb.is_null() {
        return None;
    }
    if !kread(proc, base + prva, pb, pread) {
        ExFreePoolWithTag(pb as PVOID, POOL_TAG);
        return None;
    }

    // Each .pdata entry is 12 bytes: [begin_rva:u32, end_rva:u32, unwind_info:u32]
    let entries = pread / 12;

    // Build sorted function list in NonPagedPool
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
        let b = u32::from_le_bytes(ps[i * 12..i * 12 + 4].try_into().unwrap_or([0; 4]));
        let e = u32::from_le_bytes(ps[i * 12 + 4..i * 12 + 8].try_into().unwrap_or([0; 4]));
        if b > 0 && e > b && (e as usize) <= soi {
            fs[n] = (b, e);
            n += 1;
        }
    }
    ExFreePoolWithTag(pb as PVOID, POOL_TAG);
    fs[..n].sort_unstable_by_key(|&(b, _)| b);

    // --- Step 3: Batch scan with NonPagedPool buffer ---
    let sb = ExAllocatePool2(POOL_FLAG_NON_PAGED, MAX_BATCH as SIZE_T, POOL_TAG) as *mut u8;
    if sb.is_null() {
        ExFreePoolWithTag(fb as PVOID, POOL_TAG);
        return None;
    }

    let ilen = prefix.len() + 4; // prefix + 4-byte displacement
    let ptot = ilen + suffix.len();
    let mut result = None;
    let mut i = 0;

    'out: while i < n {
        // Group adjacent functions into one batch (max 512KB, max 4KB gap)
        let bs = fs[i].0 as u64;
        let mut be = fs[i].1 as u64;
        let mut j = i + 1;
        while j < n {
            if fs[j].0 as u64 - be > BATCH_GAP || fs[j].1 as u64 - bs > MAX_BATCH as u64 {
                break;
            }
            be = fs[j].1 as u64;
            j += 1;
        }

        // Read the entire batch
        let rsz = ((be - bs) as usize).min(MAX_BATCH);
        if rsz >= ptot && kread(proc, base + bs, sb, rsz) {
            let buf = core::slice::from_raw_parts(sb, rsz);

            // Linear search for pattern within this batch
            'sc: for off in 0..rsz - ptot + 1 {
                // Match prefix
                if buf[off..off + prefix.len()] != *prefix {
                    continue;
                }
                // Match suffix (0xFF = wildcard)
                for (k, &v) in suffix.iter().enumerate() {
                    if v != 0xFF && buf[off + ilen + k] != v {
                        continue 'sc;
                    }
                }

                // --- Step 4: Compute anchor RVA from RIP-relative displacement ---
                // The instruction at (base + bs + off) is something like:
                //   MOV reg, [RIP + disp32]    (reads anchor pointer)
                // So: anchor_rva = instruction_RVA + instruction_length + disp32
                let disp = i32::from_le_bytes(
                    buf[off + prefix.len()..off + prefix.len() + 4]
                        .try_into()
                        .unwrap_or([0; 4]),
                );
                let instr_rva = bs + off as u64;
                let gw = ((instr_rva as i64) + ilen as i64 + disp as i64) as u64;

                if gw > 0 && gw < soi as u64 {
                    result = Some(gw);
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

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Global Allocator + Panic Handler
// ═══════════════════════════════════════════════════════════════════════════════

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
