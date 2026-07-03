//! WumaTracker Player Position Service
//!
//! Single-purpose kernel driver for WumaTracker player world coordinates.
//! No write path, no external scan request path, no generic memory access path,
//! and no runtime configuration surface.

#![no_std]
#![no_main]
#![allow(non_snake_case, non_camel_case_types)]

extern crate alloc;

#[path = "../shared/ioctls.rs"]
mod ioctls;

use wdk_sys::{
    ntddk::{
        ExAllocatePool2, ExFreePoolWithTag, IoCreateSymbolicLink, IoDeleteDevice,
        IoDeleteSymbolicLink, IofCompleteRequest, ObDereferenceObjectDeferDelete,
        PsLookupProcessByProcessId,
    },
    DO_BUFFERED_IO, DO_DEVICE_INITIALIZING, FILE_DEVICE_UNKNOWN, HANDLE, NTSTATUS,
    PDEVICE_OBJECT, PEPROCESS, PIRP, PUNICODE_STRING, PVOID, SIZE_T, ULONG, ULONG64, ULONG_PTR,
    UNICODE_STRING,
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
// wdm.h: MM_COPY_MEMORY_PHYSICAL = 0x1, MM_COPY_MEMORY_VIRTUAL = 0x2. This was
// previously 0x1 (the PHYSICAL flag), which told MmCopyMemory to treat every
// `MmCopyAddr.va` as a physical address instead of a virtual one — every
// single kread() failed with STATUS_PARTIAL_COPY regardless of how correct
// the target virtual address was, which is exactly the symptom that led here.
const MM_COPY_MEMORY_VIRTUAL: ULONG = 0x2;
const POOL_TAG: ULONG = u32::from_le_bytes(*b"WuDr");
const ALLOC_TAG: u32 = u32::from_le_bytes(*b"WuAl");
// WumaTracker-specific bugcheck code, used only by our own panic handler; not a
// documented Microsoft bugcheck, so it never collides with a system-defined one.
const WUMATRACKER_BUGCHECK_CODE: ULONG = u32::from_le_bytes(*b"WuBC");

const IRP_IO_STATUS: usize = 0x30;
const IRP_IO_INFORMATION: usize = 0x38;
const IRP_SYSTEM_BUFFER: usize = 0x18;
const IRP_CURRENT_STACK: usize = 0xB8;
// IO_STACK_LOCATION.Parameters.DeviceIoControl layout (Parameters starts at
// +0x08 in IO_STACK_LOCATION): OutputBufferLength, then InputBufferLength
// (POINTER_ALIGNMENT-padded to +0x08), then IoControlCode (+0x10). These were
// previously swapped (IOCTL_CODE and OUT_LEN), so dispatch_ioctl's `code`
// read OutputBufferLength instead of the real IOCTL code, meaning
// IOCTL_GET_LOCATION never matched and on_get_location never ran.
const STACK_OUT_LEN: usize = 0x08;
const STACK_IN_LEN: usize = 0x10;
const STACK_IOCTL_CODE: usize = 0x18;

const DEVICE_OBJECT_FLAGS: usize = 0x30;
const DRV_DEVICE_OBJECT: usize = 0x08;
const DRV_UNLOAD: usize = 0x68;
const DRV_MAJOR_FUNCTION: usize = 0x70;

use ioctls::IOCTL_GET_LOCATION;

const USER_MIN: u64 = 0x1_0000;
const USER_MAX: u64 = 0x7FFF_FFFF_FFFF;
const WORLD_ANCHOR_PATTERN_PREFIX: &[u8] = &[0x48, 0x8B, 0x1D];
const WORLD_ANCHOR_PATTERN_SUFFIX: &[u8] = &[0x48, 0x85, 0xDB, 0x74, 0xFF, 0x41, 0xB0, 0x01];

/// Converts an ASCII `&str` to a fixed-size UTF-16 array at compile time, so
/// `TARGET_PROCESS_NAME_UTF16` below is generated from `ioctls::TARGET_PROCESS_NAME`
/// instead of being a second, independently-maintained copy of the process name.
const fn ascii_to_utf16<const N: usize>(s: &str) -> [u16; N] {
    let bytes = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}
const TARGET_PROCESS_NAME_UTF16: [u16; 26] = ascii_to_utf16(ioctls::TARGET_PROCESS_NAME);

struct PlayerCoordConfig {
    chain: [u64; 6],
    relative_location_offset: u64,
    add_world_origin: bool,
}

static PLAYER_COORD_CONFIG: PlayerCoordConfig = PlayerCoordConfig {
    chain: [0x180, 0x38, 0, 0x30, 0x2A0, 0x130],
    relative_location_offset: 0x11C,
    add_world_origin: false,
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
    fn MmGetSystemRoutineAddress(SystemRoutineName: PUNICODE_STRING) -> PVOID;
    // Long-stable ntoskrnl.exe export (used by the same class of tooling as
    // Process Hacker/System Informer) that returns EPROCESS::SectionBaseAddress
    // directly. Preferred over walking Peb->ImageBaseAddress: it needs no
    // MmCopyMemory into the target's user address space (no attach, no risk
    // of a partial/failed copy from that page), and it's immune to the
    // Peb->ImageBaseAddress vs Wow64Process->Peb32->ImageBaseAddress split
    // that WOW64 processes have.
    fn PsGetProcessSectionBaseAddress(Process: PEPROCESS) -> *mut u8;
    // Long-stable ntoskrnl.exe export returning a pointer to a fixed 16-byte
    // (15 chars + NUL) buffer inside EPROCESS holding the truncated image
    // base name. Used only to cheaply confirm a cached PID still refers to
    // the expected process (PID reuse after the original process exits is
    // the failure mode this guards against) without redoing the full
    // ZwQuerySystemInformation process-list scan.
    fn PsGetProcessImageFileName(Process: PEPROCESS) -> *mut u8;
    // Long-stable ntoskrnl.exe export; the mirror of PsLookupProcessByProcessId,
    // used to read back the PID of a process found via the full scan so it
    // can be cached.
    fn PsGetProcessId(Process: PEPROCESS) -> HANDLE;
    // Not part of the wdk-sys ntddk.h binding subset, but a long-documented
    // (if lightly-documented) NT export used here instead of the undocumented
    // PsGetNextProcess, which this Windows build's ntoskrnl.exe does not
    // export at all (confirmed via `dumpbin /exports`).
    fn ZwQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: PVOID,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
    fn KeStackAttachProcess(Process: PEPROCESS, ApcState: *mut u8);
    fn KeUnstackDetachProcess(ApcState: *mut u8);
    fn KeBugCheckEx(
        BugCheckCode: ULONG,
        BugCheckParameter1: ULONG_PTR,
        BugCheckParameter2: ULONG_PTR,
        BugCheckParameter3: ULONG_PTR,
        BugCheckParameter4: ULONG_PTR,
    ) -> !;
}

static DEVICE_NT: &[u16] = &[
    0x5C, 0x44, 0x65, 0x76, 0x69, 0x63, 0x65, 0x5C, 0x57, 0x75, 0x6D, 0x61, 0x44, 0x69, 0x73, 0x70,
    0x6C, 0x61, 0x79, 0x53, 0x65, 0x72, 0x76, 0x69, 0x63, 0x65, 0,
];
static DEVICE_DOS: [u16; 31] = ascii_to_utf16(ioctls::DEVICE_SYMBOLIC_LINK_NAME);
// SDDL "D:P(A;;GA;;;BA)(A;;GA;;;SY)": Administrators (BA) and SYSTEM (SY) get
// generic-all access; everyone else is denied. Protected (P) so it cannot be
// widened by an inherited ACE.
static DEVICE_SDDL: &[u16] = &[
    0x44, 0x3A, 0x50, 0x28, 0x41, 0x3B, 0x3B, 0x47, 0x41, 0x3B, 0x3B, 0x3B, 0x42, 0x41, 0x29, 0x28,
    0x41, 0x3B, 0x3B, 0x47, 0x41, 0x3B, 0x3B, 0x3B, 0x53, 0x59, 0x29, 0,
];
// IoCreateDeviceSecure is a real WDM export (documented since Vista) but is
// absent from this WDK's ntoskrnl.lib/wdmsec.lib import stubs, so it cannot
// be statically linked here. Resolved dynamically at runtime instead.
const IO_CREATE_DEVICE_SECURE_NAME: [u16; 21] = ascii_to_utf16("IoCreateDeviceSecure");

type IoCreateDeviceSecureFn = unsafe extern "system" fn(
    *mut u8,
    ULONG,
    PUNICODE_STRING,
    ULONG,
    ULONG,
    u8,
    PUNICODE_STRING,
    PVOID,
    *mut PDEVICE_OBJECT,
) -> NTSTATUS;

use ioctls::GetLocationResponse;

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

#[repr(C)]
struct FVector {
    x: f32,
    y: f32,
    z: f32,
}

#[repr(C)]
struct FRotator {
    pitch: f32,
    yaw: f32,
    roll: f32,
}

#[repr(C)]
struct FRelativeTransform {
    location: FVector,
    rotation: FRotator,
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
    let Some(io_create_device_secure) = resolve_io_create_device_secure() else {
        return STATUS_UNSUCCESSFUL;
    };

    let mut nt = ustr(DEVICE_NT);
    let mut dos = ustr(&DEVICE_DOS);
    let mut sddl = ustr(DEVICE_SDDL);
    let mut dev: PDEVICE_OBJECT = core::ptr::null_mut();

    let s = io_create_device_secure(
        drv,
        0,
        &mut nt,
        FILE_DEVICE_UNKNOWN,
        0,
        0,
        &mut sddl,
        core::ptr::null_mut(),
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

    let flags = (dev as *mut u8).add(DEVICE_OBJECT_FLAGS) as *mut u32;
    *flags = (*flags | DO_BUFFERED_IO) & !DO_DEVICE_INITIALIZING;

    let mf = drv.add(DRV_MAJOR_FUNCTION) as *mut usize;
    *mf.add(0) = dispatch_ok as *const () as usize;
    *mf.add(2) = dispatch_ok as *const () as usize;
    *mf.add(14) = dispatch_ioctl as *const () as usize;
    *(drv.add(DRV_UNLOAD) as *mut usize) = unload as *const () as usize;

    STATUS_SUCCESS
}

unsafe extern "system" fn unload(drv: *mut u8) {
    let mut dos = ustr(&DEVICE_DOS);
    let _ = IoDeleteSymbolicLink(&mut dos);
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
    // KAPC_STATE is made up of pointer-sized/LIST_ENTRY fields that the
    // kernel's attach/detach routines assume are naturally aligned. A plain
    // `[u8; N]` only guarantees 1-byte alignment, so on an unlucky stack
    // layout KeStackAttachProcess's internal bookkeeping can end up
    // corrupted, silently failing to actually attach — every subsequent
    // MmCopyMemory then runs in the *caller's* address space instead of the
    // target's, failing regardless of how correct `addr` is.
    #[repr(C, align(16))]
    struct ApcState([u8; 72]);
    let mut apc = ApcState([0u8; 72]);
    KeStackAttachProcess(proc, apc.0.as_mut_ptr());
    let mut copied: SIZE_T = 0;
    let s = MmCopyMemory(
        out as PVOID,
        MmCopyAddr { va: addr as PVOID },
        len as SIZE_T,
        MM_COPY_MEMORY_VIRTUAL,
        &mut copied,
    );
    KeUnstackDetachProcess(apc.0.as_mut_ptr());
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
    if olen < core::mem::size_of::<GetLocationResponse>() || buf.is_null() {
        return (STATUS_BUFFER_TOO_SMALL, 0);
    }

    let r = &mut *(buf as *mut GetLocationResponse);
    *r = GetLocationResponse {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
        stage: 0,
        _pad: [0; 3],
    };

    let proc = match find_target_process_cached() {
        Ok(proc) => proc,
        Err(dbg) => {
            // Bit-exact (not lossy-cast) so the real i32/u32 values survive
            // the f32 field round-trip for inspection without a debugger.
            r.x = f32::from_bits(dbg.status as u32);
            r.y = f32::from_bits(dbg.actual_len);
            r.z = f32::from_bits(dbg.first_next_entry_offset);
            r.pitch = f32::from_bits(dbg.seen);
            r.yaw = f32::from_bits(dbg.first_dword_after_call);
            r.stage = 0xFE;
            return (STATUS_SUCCESS, core::mem::size_of::<GetLocationResponse>());
        }
    };

    let result = read_player_world_position(proc);
    ObDereferenceObjectDeferDelete(proc as PVOID);

    match result {
        Ok((x, y, z, p, yw, rl)) => {
            r.x = x;
            r.y = y;
            r.z = z;
            r.pitch = p;
            r.yaw = yw;
            r.roll = rl;
            (STATUS_SUCCESS, core::mem::size_of::<GetLocationResponse>())
        }
        Err(stage) => {
            // Buffered I/O only copies the system buffer back to the caller
            // when the IRP completes with a success status, so a non-success
            // status here would discard `stage` entirely (DeviceIoControl
            // reports 0 bytes returned and Win32 error 31/ERROR_GEN_FAILURE).
            // The real per-stage failure is carried in-band via `stage`
            // instead, so the IOCTL itself always succeeds.
            r.stage = stage;
            (STATUS_SUCCESS, core::mem::size_of::<GetLocationResponse>())
        }
    }
}

// SYSTEM_INFORMATION_CLASS::SystemProcessInformation.
const SYSTEM_PROCESS_INFORMATION_CLASS: u32 = 5;
const STATUS_INFO_LENGTH_MISMATCH: NTSTATUS = 0xC000_0004u32 as i32;

// Offsets into each SYSTEM_PROCESS_INFORMATION record. Stable across Windows
// versions for this portion of the struct (relied on by process-listing
// tools like Process Hacker/System Informer for well over a decade):
//   +0x00 NextEntryOffset : ULONG
//   +0x38 ImageName       : UNICODE_STRING (bare file name, not truncated)
//   +0x50 UniqueProcessId : HANDLE
const SPI_NEXT_ENTRY_OFFSET: usize = 0x00;
const SPI_IMAGE_NAME: usize = 0x38;
const SPI_UNIQUE_PROCESS_ID: usize = 0x50;

/// Enumerates processes via `ZwQuerySystemInformation(SystemProcessInformation)`
/// and looks up the first one whose image name matches the hardcoded target,
/// returning a referenced `PEPROCESS` via `PsLookupProcessByProcessId`.
///
/// `PsGetNextProcess` (the more commonly seen approach in community driver
/// samples) is deliberately not used here: it is an undocumented internal
/// routine, and this Windows build's ntoskrnl.exe does not export it at all
/// (confirmed via `dumpbin /exports`), so relying on it would silently
/// prevent the target process from ever being found on such builds. This
/// matches the documented approach the driver plan calls for.
/// Temporary rich diagnostics for find_target_process's failure path,
/// surfaced bit-exact (via f32::from_bits, not a lossy numeric cast) through
/// the response's x/y/z/pitch fields so it can be inspected without a kernel
/// debugger attached. Remove once process lookup is confirmed working.
#[derive(Clone, Copy)]
struct FindProcessDebug {
    status: NTSTATUS,
    actual_len: u32,
    first_next_entry_offset: u32,
    seen: u32,
    /// buf's first dword right after the ZwQuerySystemInformation call.
    /// Compared against the 0xCAFEBABE canary written just before the call:
    /// if it's still 0xCAFEBABE, the call never actually wrote to the
    /// buffer at all.
    first_dword_after_call: u32,
}

// Across-call caches so a live overlay polling this IOCTL doesn't redo a
// full ZwQuerySystemInformation process-list scan and a world-anchor byte-pattern
// scan (up to 512KB read from the target) on every single request. Both are
// self-healing: a cache hit is verified cheaply before use, and any failure
// (process exited/PID reused, module reloaded/relocated) falls back to a
// full rescan on that same call and refreshes the cache.
static CACHED_PID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static CACHED_WORLD_ANCHOR_RVA: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

// First 14 bytes of "Client-Win64-Shipping.exe" — EPROCESS::ImageFileName is
// a fixed UCHAR[15] (truncated basename, not guaranteed NUL-terminated for
// names this long), so 14 bytes is the most we can safely compare.
const CACHED_NAME_PREFIX: &[u8; 14] = b"Client-Win64-S";

/// Cheaply re-validates a cached PID instead of redoing the full process
/// scan: looks the PID up directly and confirms it still refers to the
/// expected image, guarding against the PID having been reused by an
/// unrelated process after the original one exited.
unsafe fn try_cached_process() -> Option<PEPROCESS> {
    let pid = CACHED_PID.load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 {
        return None;
    }
    let mut proc: PEPROCESS = core::ptr::null_mut();
    if PsLookupProcessByProcessId(pid as HANDLE, &mut proc) != STATUS_SUCCESS || proc.is_null() {
        CACHED_PID.store(0, core::sync::atomic::Ordering::Relaxed);
        return None;
    }
    let name_ptr = PsGetProcessImageFileName(proc);
    let matches = !name_ptr.is_null()
        && core::slice::from_raw_parts(name_ptr, CACHED_NAME_PREFIX.len()) == CACHED_NAME_PREFIX;
    if !matches {
        ObDereferenceObjectDeferDelete(proc as PVOID);
        CACHED_PID.store(0, core::sync::atomic::Ordering::Relaxed);
        return None;
    }
    Some(proc)
}

/// Cache-aware wrapper around `find_target_process`: tries the cached PID
/// first, only falling back to the full process-list scan on a cache miss
/// or a stale/invalid cached PID, and refreshes the cache on a fresh scan.
unsafe fn find_target_process_cached() -> Result<PEPROCESS, FindProcessDebug> {
    if let Some(proc) = try_cached_process() {
        return Ok(proc);
    }
    let result = find_target_process();
    if let Ok(proc) = result {
        CACHED_PID.store(
            PsGetProcessId(proc) as u64,
            core::sync::atomic::Ordering::Relaxed,
        );
    }
    result
}

/// Cache-aware world-anchor resolution: tries the cached RVA first (just a
/// pointer read, not a rescan) and only falls back to the full byte-pattern
/// scan if that fails (module reloaded/relocated), refreshing the cache on
/// a fresh scan.
unsafe fn resolve_world_anchor(proc: PEPROCESS, base: u64) -> Option<u64> {
    let cached = CACHED_WORLD_ANCHOR_RVA.load(core::sync::atomic::Ordering::Relaxed);
    if cached != 0 {
        if let Some(anchor) = kread_ptr(proc, base + cached) {
            return Some(anchor);
        }
    }
    let rva = find_hardcoded_world_anchor(proc, base)?;
    let anchor = kread_ptr(proc, base + rva)?;
    CACHED_WORLD_ANCHOR_RVA.store(rva, core::sync::atomic::Ordering::Relaxed);
    Some(anchor)
}

unsafe fn find_target_process() -> Result<PEPROCESS, FindProcessDebug> {
    let mut buf_len: u32 = 64 * 1024;
    let mut buf = ExAllocatePool2(POOL_FLAG_NON_PAGED, buf_len as SIZE_T, POOL_TAG) as *mut u8;
    if buf.is_null() {
        return Err(FindProcessDebug {
            status: -1,
            actual_len: 0,
            first_next_entry_offset: 0,
            seen: 0,
            first_dword_after_call: 0,
        });
    }

    // Canaries to tell "the call never touched these" apart from "the call
    // legitimately wrote zero": if these come back unchanged, the call site
    // isn't actually reaching the real routine at all.
    let mut actual_len: u32 = 0xDEAD_BEEF;
    *(buf as *mut u32) = 0xCAFE_BABE;
    let mut status = ZwQuerySystemInformation(
        SYSTEM_PROCESS_INFORMATION_CLASS,
        buf as PVOID,
        buf_len,
        &mut actual_len,
    );
    let first_dword_after_call = *(buf as *const u32);
    let mut retries = 0;
    while status == STATUS_INFO_LENGTH_MISMATCH && retries < 8 {
        ExFreePoolWithTag(buf as PVOID, POOL_TAG);
        buf_len = actual_len.saturating_add(4096);
        buf = ExAllocatePool2(POOL_FLAG_NON_PAGED, buf_len as SIZE_T, POOL_TAG) as *mut u8;
        if buf.is_null() {
            return Err(FindProcessDebug {
                status: -2,
                actual_len,
                first_next_entry_offset: 0,
                seen: 0,
                first_dword_after_call: 0,
            });
        }
        status = ZwQuerySystemInformation(
            SYSTEM_PROCESS_INFORMATION_CLASS,
            buf as PVOID,
            buf_len,
            &mut actual_len,
        );
        retries += 1;
    }

    if status != STATUS_SUCCESS {
        ExFreePoolWithTag(buf as PVOID, POOL_TAG);
        return Err(FindProcessDebug {
            status,
            actual_len,
            first_next_entry_offset: 0,
            seen: 0,
            first_dword_after_call,
        });
    }

    let first_next_entry_offset = *(buf.add(SPI_NEXT_ENTRY_OFFSET) as *const u32);

    let mut seen = 0u32;
    let mut offset = 0usize;
    let mut matched_pid: Option<HANDLE> = None;
    loop {
        let entry = buf.add(offset);
        let next_entry_offset = *(entry.add(SPI_NEXT_ENTRY_OFFSET) as *const u32);
        let image_name = &*(entry.add(SPI_IMAGE_NAME) as *const UNICODE_STRING);

        if !image_name.Buffer.is_null() {
            seen = seen.wrapping_add(1);
            if unicode_path_ends_with_target(image_name) {
                matched_pid = Some(*(entry.add(SPI_UNIQUE_PROCESS_ID) as *const HANDLE));
                break;
            }
        }

        if next_entry_offset == 0 {
            break;
        }
        offset += next_entry_offset as usize;
    }

    ExFreePoolWithTag(buf as PVOID, POOL_TAG);

    let Some(pid) = matched_pid else {
        return Err(FindProcessDebug {
            status,
            actual_len,
            first_next_entry_offset,
            seen,
            first_dword_after_call,
        });
    };

    let mut proc: PEPROCESS = core::ptr::null_mut();
    let lookup_status = PsLookupProcessByProcessId(pid, &mut proc);
    if lookup_status != STATUS_SUCCESS || proc.is_null() {
        return Err(FindProcessDebug {
            status: lookup_status,
            actual_len,
            first_next_entry_offset,
            seen,
            first_dword_after_call,
        });
    }
    Ok(proc)
}

unsafe fn resolve_io_create_device_secure() -> Option<IoCreateDeviceSecureFn> {
    let mut name = ustr(&IO_CREATE_DEVICE_SECURE_NAME);
    let routine = MmGetSystemRoutineAddress(&mut name);
    if routine.is_null() {
        None
    } else {
        Some(core::mem::transmute::<PVOID, IoCreateDeviceSecureFn>(
            routine,
        ))
    }
}

unsafe fn unicode_path_ends_with_target(name: &UNICODE_STRING) -> bool {
    let len = (name.Length / 2) as usize;
    let target_len = TARGET_PROCESS_NAME_UTF16.len() - 1;
    if name.Buffer.is_null() || len < target_len {
        return false;
    }
    let chars = core::slice::from_raw_parts(name.Buffer, len);
    let start = len - target_len;
    for i in 0..target_len {
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
    let anchor = resolve_world_anchor(proc, base).ok_or(1u8)?;

    let mut ptr = anchor;
    for i in 0..PLAYER_COORD_CONFIG.chain.len() {
        ptr = kread_ptr(proc, ptr + PLAYER_COORD_CONFIG.chain[i]).ok_or((i + 2) as u8)?;
    }

    let mut rel = core::mem::MaybeUninit::<FRelativeTransform>::uninit();
    if !kread(
        proc,
        ptr + PLAYER_COORD_CONFIG.relative_location_offset,
        rel.as_mut_ptr() as *mut u8,
        core::mem::size_of::<FRelativeTransform>(),
    ) {
        return Err((PLAYER_COORD_CONFIG.chain.len() + 2) as u8);
    }
    let rel = rel.assume_init();

    let iv = if PLAYER_COORD_CONFIG.add_world_origin {
        let persistent_level =
            kread_ptr(proc, anchor + 0x30).ok_or((PLAYER_COORD_CONFIG.chain.len() + 3) as u8)?;
        let mut iv = core::mem::MaybeUninit::<FIntVector>::uninit();
        if !kread(
            proc,
            persistent_level + 0xC8,
            iv.as_mut_ptr() as *mut u8,
            core::mem::size_of::<FIntVector>(),
        ) {
            return Err((PLAYER_COORD_CONFIG.chain.len() + 4) as u8);
        }
        iv.assume_init()
    } else {
        FIntVector { x: 0, y: 0, z: 0 }
    };

    Ok((
        rel.location.x + iv.x as f32,
        rel.location.y + iv.y as f32,
        rel.location.z + iv.z as f32,
        rel.rotation.pitch,
        rel.rotation.yaw,
        rel.rotation.roll,
    ))
}

unsafe fn read_main_module_base(proc: PEPROCESS) -> Option<u64> {
    // Reads EPROCESS::SectionBaseAddress directly instead of walking
    // Peb->ImageBaseAddress: no attach, no MmCopyMemory into the target's
    // user address space, so it can't fail with STATUS_PARTIAL_COPY the way
    // that path did.
    let raw = PsGetProcessSectionBaseAddress(proc) as u64;
    if !is_valid_ptr(raw) {
        return None;
    }
    Some(raw)
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

unsafe fn find_hardcoded_world_anchor(proc: PEPROCESS, base: u64) -> Option<u64> {
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

    let instruction_len = WORLD_ANCHOR_PATTERN_PREFIX.len() + 4;
    let pattern_len = instruction_len + WORLD_ANCHOR_PATTERN_SUFFIX.len();
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
                if scan[off..off + WORLD_ANCHOR_PATTERN_PREFIX.len()] != *WORLD_ANCHOR_PATTERN_PREFIX {
                    continue;
                }
                for (k, &v) in WORLD_ANCHOR_PATTERN_SUFFIX.iter().enumerate() {
                    if v != 0xFF && scan[off + instruction_len + k] != v {
                        continue 'scan;
                    }
                }

                let Some(disp) = read_i32(scan, off + WORLD_ANCHOR_PATTERN_PREFIX.len()) else {
                    continue;
                };
                let instr_rva = batch_start + off as u64;
                let world_anchor_rva = ((instr_rva as i64) + instruction_len as i64 + disp as i64) as u64;
                if world_anchor_rva > 0 && world_anchor_rva < soi as u64 {
                    result = Some(world_anchor_rva);
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
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        KeBugCheckEx(
            WUMATRACKER_BUGCHECK_CODE,
            info as *const _ as ULONG_PTR,
            0,
            0,
            0,
        )
    }
}
