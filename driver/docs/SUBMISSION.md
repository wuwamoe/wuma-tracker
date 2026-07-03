---
title: WumaTracker Driver Submission Package
status: canonical
location: driver/docs/SUBMISSION.md
based-on: driver/docs/PLAN.md, driver/docs/TASKS.md
last-updated: 2026-07-03
---

# WumaTracker Driver Submission Package

This document explains the driver system's legitimate single purpose and its
security posture without requiring a reviewer to read the full codebase.

## Purpose Statement

The WumaTracker kernel driver (`WumaDisplayService`) reads only the current
player's world position (location and rotation) from the WumaTracker game
client process, for the sole purpose of feeding the WumaTracker overlay
service. It has no write path into any process, no caller-supplied pattern
scan, no generic memory read primitive, and no runtime configuration surface.
The driver's own binary carries this statement as a durable, `strings`-visible
constant (see "Purpose string" below).

## Architecture Summary

Three components, in trust order from lowest to highest privilege:

1. **Tauri app** (`src-tauri/`) — a normal, unprivileged user process. It never
   opens `\\.\WumaDisplayService`. It sends exactly one request type ("get
   current location") to the helper over a named pipe and renders the result.
2. **Helper service** (`driver/helper/`) — the only normal-operation component
   that opens the driver device. It exposes a single IPC operation over
   `\\.\pipe\WumaTrackerHelper`, forwards it to `IOCTL_GET_LOCATION`, and
   returns the raw coordinate response. It never forwards arbitrary IOCTLs and
   never accepts a PID, base address, pointer chain, or pattern bytes from a
   client.
3. **Kernel driver** (`driver/`) — a WDM driver exposing exactly one IOCTL,
   `IOCTL_GET_LOCATION`. It internally, and only internally, resolves the
   hardcoded target process name, walks a hardcoded pointer chain, and returns
   coordinates. All chain/offset/process-name values are compiled in; nothing
   is accepted from a caller at any layer.

```
Tauri app --(named pipe, get-location only)--> Helper --(IOCTL_GET_LOCATION)--> Driver
```

## Helper Binary Integrity

Release builds of the helper are Authenticode-signed with the project's
production signing certificate (see `driver/scripts/build-and-sign.ps1`,
which signs both the driver `.sys` and the helper `.exe` with the same
certificate and timestamps both).

At startup, the helper verifies its own signature via `WinVerifyTrust`
(`WINTRUST_ACTION_GENERIC_VERIFY_V2` against its own `current_exe()` path) and
refuses to start if verification fails — this check runs unconditionally in
release builds and is compiled out only in debug builds, where local
iteration does not require a production signing certificate. See
`verify_self_signature` in `driver/helper/src/main.rs`.

## Driver IOCTL List

The driver exposes exactly one IOCTL:

- `IOCTL_GET_LOCATION` — defined once in `driver/shared/ioctls.rs`, the sole
  source of truth for the on-wire ABI. Takes no request payload; returns a
  fixed `GetLocationResponse` struct (`x, y, z, pitch, yaw, roll, stage`).

There is no `IOCTL_AUTH`, `IOCTL_SET_CONFIG`, or `IOCTL_PATTERN_SEARCH` in the
final driver.

## Device SDDL

The device object is created with `IoCreateDeviceSecure` using the SDDL
string:

```
D:P(A;;GA;;;BA)(A;;GA;;;SY)
```

Administrators (`BA`) and SYSTEM (`SY`) get generic-all access; the `P` flag
protects the descriptor from being widened by an inherited ACE. No other
account, including the interactive user account the helper and app normally
run under, can open the device directly — only the (Administrator-elevated)
helper process can.

## Helper IPC Security Descriptor

The helper's named pipe (`\\.\pipe\WumaTrackerHelper`) is created per
connection with a security descriptor built as:

```
D:P(A;;GRGW;;;BA)(A;;GRGW;;;<helper's own user SID>)
```

Administrators and the specific Windows account the helper process is running
under get read/write access; everyone else — including `Everyone` and
`Authenticated Users` — is denied. The pipe also rejects remote clients
(`PIPE_REJECT_REMOTE_CLIENTS`).

## Target Process Name

The driver hardcodes the target process name as `Client-Win64-Shipping.exe`
(`driver/shared/ioctls.rs::TARGET_PROCESS_NAME`, imported by both the driver
and — transitively, for documentation purposes — the helper). The driver's
own copy used for the UTF-16 process-name comparison
(`TARGET_PROCESS_NAME_UTF16` in `driver/src/main.rs`) is generated from this
same constant at compile time, not maintained as an independent literal.

Process lookup enumerates processes via `PsGetNextProcess` and matches the
full image name returned by `SeLocateProcessImageName` (not the
possibly-truncated name from `PsGetProcessImageFileName`), so a process named
merely similarly cannot be mistaken for the target.

## Purpose String

The driver binary carries a `#[used]` static string in the `.rdata` section
(`WUMATRACKER_PURPOSE` in `driver/src/main.rs`) containing the phrase
`WumaTracker Player Position Service` along with an explicit statement of what
the driver does not do. Being `#[used]` and placed in a real data section
(not debug info), it survives a normal release build and stripping and is
discoverable with `strings WumaDisplayService.sys`.

## Maintenance Note

The pointer chain, world-anchor pattern/RVA, and target process name
are all specific to a given game client build. A game update that changes the
executable name, module layout, or pointer chain requires a new build of the
driver with updated `PlayerCoordConfig` values in `driver/src/main.rs`
(kernel-mode changes require a new signed driver; there is no runtime
reconfiguration path by design).
