# Agent Guidelines for wuma-tracker-driver

This file provides context for AI agents working on this codebase.

## Project Type

Windows kernel-mode driver (WDM) written in Rust using `wdk-sys` for type definitions and direct `extern "system"` declarations for kernel APIs not exposed by the crate.

## Key Constraints

- **`#![no_std]`** — no standard library. Use `core`, `alloc`, and `libm` only.
- **Stack limit** — kernel stack is ~24KB. Never allocate large buffers on stack. Use `ExAllocatePool2` (NonPagedPool) for anything over ~1KB.
- **No panics in production** — panic handler loops forever. Avoid unwrap/expect. Use `Option`/`Result` patterns.
- **No floating point in older Windows kernels** — we use `libm` for trig functions. The target (Win10+) supports FP in kernel, but avoid heavy FP in DPC/ISR context.
- **IRP offsets are raw** — `wdk-sys` 0.3.0 provides opaque structs for IRP/DRIVER_OBJECT, so we access fields via known byte offsets. These are x64-only and Windows 10+ specific.

## File Structure

Single file: `src/main.rs` — intentionally monolithic for kernel driver simplicity.

Sections:
1. Types / constants / extern declarations
2. Global session state (atomics + unsafe static config)
3. DriverEntry + IOCTL dispatch boilerplate
4. Business logic (kread, walk_chain, scan_gworld)
5. Global allocator + panic handler

## IOCTL Design

- All IOCTLs use `METHOD_BUFFERED` — kernel copies buffers, no raw user pointers.
- Input and output share `SystemBuffer` (same pointer).
- Authentication: `IOCTL_AUTH` verifies caller PID matches kernel's `PsGetCurrentProcessId()`.
- All subsequent IOCTLs require the session token returned by AUTH.

## Adding a New IOCTL

1. Add `const IOCTL_XXX: u32 = ctl(0x8XX);` in section 1
2. Add match arm in `dispatch_ioctl`
3. Implement `on_xxx(buf, ilen, olen) -> (NTSTATUS, usize)` in section 4
4. Document request/response layout in comments above the function
5. Update `win_proc_driver.rs` in the tracker crate with matching client code

## Testing

No automated kernel tests. Testing process:
1. `cargo check` — verify compilation
2. `cargo build --release` — produce binary
3. Rename `.exe` to `.sys`, sign with test cert
4. Load with `sc create` + `sc start` (requires test signing or Secure Boot off)
5. Run tracker against live game, verify coordinates

## Common Pitfalls

- Forgetting `ObDereferenceObjectDeferDelete` after `PsLookupProcessByProcessId` → handle leak
- Reading beyond buffer (`ilen`/`olen` checks must come first)
- Using `usize` where wdk-sys expects `SIZE_T` (= `u64` on x64) — always cast
- `KeStackAttachProcess` without matching `KeUnstackDetachProcess` → deadlock/BSOD
- Writing to `SystemBuffer` before reading all input (they share the same pointer for METHOD_BUFFERED)
