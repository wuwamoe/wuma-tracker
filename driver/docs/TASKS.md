---
title: WumaTracker Driver Hardening Implementation Plan
status: canonical
location: driver/docs/TASKS.md
based-on: driver/docs/PLAN.md
last-updated: 2026-07-03
---

# WumaTracker Driver Hardening Implementation Plan

This document is the execution plan for `driver/docs/PLAN.md`. It is intentionally code-facing and specific. Requirements belong in `PLAN.md`; implementation order, file edits, verification, and risk controls belong here.

Helper service location decision: create the privileged helper service under `driver/helper/`, because it is driver-owned, shipped with the driver, and must be the only normal component that opens `\\.\WumaDisplayService`.

## Current Baseline

The current driver and Windows client still expose a configurable driver protocol:
- `driver/src/main.rs` defines `IOCTL_AUTH`, `IOCTL_SET_CONFIG`, `IOCTL_GET_LOCATION`, and `IOCTL_PATTERN_SEARCH`.
- `driver/src/main.rs` stores session state in `SESSION_TOKEN`, `AUTHED_PID`, `CONFIG_READY`, and `static mut CONFIG`.
- `driver/src/main.rs` creates the device with plain `IoCreateDevice`.
- `driver/src/main.rs` has a panic handler that loops forever.
- `src-tauri/src/win_proc_driver.rs` opens `\\.\WumaDisplayService` directly and sends auth/config/pattern-scan IOCTLs.
- `driver/shared/ioctls.rs` is the Milestone 0 source of truth for the final single-IOCTL ABI.

The final state removes that protocol entirely. The driver exposes only `IOCTL_GET_LOCATION`, and the app reaches it only through a helper service.

## Implementation Rules

- Keep `PLAN.md` requirements-only. Do not add task lists, line-number implementation notes, or migration stages there.
- Update `PLAN-KO.md` after the English requirements are stable.
- Keep all new code identifiers, comments, and binary-visible strings in English first. Add Korean only as a short optional reviewer aid.
- Do not publish real game offset values in docs.
- Do not introduce `as any`, `@ts-ignore`, panics, unwraps, or broad catch-all error suppression.
- For kernel code, avoid stack buffers over approximately 1 KB.
- Every task that touches kernel object references must audit all dereference paths before it is considered complete.

## Milestone 0: Lock Final ABI and Inputs

Goal: remove ambiguity before editing code.

1. Confirm the final Windows target process name.
   - Source of truth today: `src-tauri/src/lib.rs` uses `Client-Win64-Shipping.exe`.
   - Record this exact string in code constants and submission docs.
   - Do not use the older variants `Client Win64 Shipping.exe` or `ClientWin64Shipping.exe`.

2. Define the final wire ABI in `driver/shared/ioctls.rs`.
   - Keep only `IOCTL_GET_LOCATION`.
   - Define one response struct for `x`, `y`, `z`, `pitch`, `yaw`, `roll`, and `stage`.
   - Define whether the request is zero bytes or a fixed empty request struct. Use the same choice in driver, helper, and tests.
   - Remove shared definitions for auth, config, and pattern scan.

3. Decide where the helper service crate lives.
   - Preferred: `driver/helper/` if it is driver-owned and shipped with the driver.
   - Acceptable: `src-tauri/src/driver_helper.rs` only if it is not a real Windows service.
   - Record the decision at the top of this file before implementing the helper.

Verification:
- `rg -n "IOCTL_AUTH|IOCTL_SET_CONFIG|IOCTL_PATTERN_SEARCH" driver/shared/ioctls.rs` returns no matches.
- `driver/shared/ioctls.rs` has exactly one IOCTL constant.

## Milestone 1: Make the Driver Single-Purpose

Goal: remove runtime configurability and expose only one driver operation.

Primary file: `driver/src/main.rs`.

1. Replace the module banner.
   - State that the driver only reads WumaTracker player world coordinates.
   - State that it has no write path, no caller-supplied pattern scan path, no generic memory access path, and no runtime configuration surface.

2. Add binary-visible purpose metadata.
   - Use a Rust-valid representation, for example a `#[used]` static `&str`.
   - Do not put Korean text directly inside a `b"..."` literal unless it is UTF-8 byte-escaped.
   - Include the phrase `WumaTracker Player Position Service`.

3. Replace `SessionConfig` with immutable coordinate configuration.
   - Rename to `PlayerCoordConfig`.
   - Store only compile-time constants or immutable statics.
   - Include the hardcoded GWorld anchor strategy, GWorld byte pattern, player transform chain, world-origin chain, and offsets.
   - Leave placeholder constants only if the real values are intentionally excluded from docs; the code must receive real values before release.

4. Remove session state.
   - Delete `SESSION_TOKEN`.
   - Delete `AUTHED_PID`.
   - Delete `CONFIG_READY`.
   - Delete `static mut CONFIG`.
   - Delete `verify_token`.

5. Remove obsolete IOCTLs and handlers.
   - Delete `IOCTL_AUTH`.
   - Delete `IOCTL_SET_CONFIG`.
   - Delete `IOCTL_PATTERN_SEARCH`.
   - Delete `on_auth`.
   - Delete `on_set_config`.
   - Delete `on_pattern_search`.
   - Keep pattern scanning only as an internal GWorld anchor helper with hardcoded pattern constants.
   - Rename and narrow `scan_anchor` to make it clear it cannot scan caller-supplied patterns, ranges, or processes.
   - Update `dispatch_ioctl` so only `IOCTL_GET_LOCATION` is accepted.

6. Rename and narrow the coordinate reader.
   - Rename `walk_chain` to `read_player_world_position`.
   - Remove the `SessionConfig` parameter.
   - Read only from `PlayerCoordConfig`.
   - Preserve the diagnostic `stage` behavior for chain failures.

7. Update `on_get_location`.
   - Accept only the final ABI selected in Milestone 0.
   - Do not read a token or PID from the caller.
   - Resolve the target process internally.
   - Always dereference the target process before returning.

Verification:
- `rg -n "IOCTL_AUTH|IOCTL_SET_CONFIG|IOCTL_PATTERN_SEARCH|SESSION_TOKEN|AUTHED_PID|CONFIG_READY|static mut CONFIG|verify_token|on_auth|on_set_config|on_pattern_search|walk_chain" driver/src/main.rs` returns no matches, except intentional historical comments are not allowed.
- `rg -n "prefix_len|suffix_len|PatternSearch|pattern.*buf|caller.*pattern" driver/src/main.rs` returns no live caller-configurable pattern-scan path.
- `rg -n "PsLookupProcessByProcessId\\(|pid" driver/src/main.rs` shows no user-controlled PID path.
- `cargo check` in `driver/` succeeds.

## Milestone 2: Implement Safe Target Process Resolution

Goal: locate only `Client-Win64-Shipping.exe` without accepting a PID from user mode.

Primary file: `driver/src/main.rs`.

1. Add `TARGET_PROCESS_NAME`.
   - Use exactly `Client-Win64-Shipping.exe`.
   - Store it in a representation suitable for the chosen lookup API.

2. Implement `find_target_process`.
   - It must return a referenced `PEPROCESS` only to internal driver code.
   - It must never expose a kernel pointer to user mode.
   - It must distinguish "not found" from "lookup failed" where practical.

3. Avoid truncated-name traps.
   - Do not rely only on `PsGetProcessImageFileName` if its available name buffer cannot represent the full target string.
   - If using `ZwQuerySystemInformation(SystemProcessInformation)`, document the buffer allocation, retry, and parsing rules in comments near the function.
   - If using process IDs from system-process enumeration followed by `PsLookupProcessByProcessId`, dereference every successful lookup that is not returned.

4. Audit object lifetime.
   - The successful `PEPROCESS` returned by `find_target_process` must be dereferenced by the caller.
   - Every failure path after a successful lookup must dereference before continuing.

Verification:
- Code review checklist in the PR includes every `PEPROCESS` acquisition and matching dereference.
- A test build with the target process absent returns the defined "target not found" error.
- A test build with a similarly named process does not match unless the full expected name matches.

## Milestone 3: Harden Driver Entry, Device Security, and Panic Behavior

Goal: make the remaining driver surface restrictive and diagnosable.

Primary file: `driver/src/main.rs`.

1. Replace `IoCreateDevice` with secure device creation.
   - Use `IoCreateDeviceSecure` or the safest available WDM equivalent in this toolchain.
   - Apply an Administrators + SYSTEM only descriptor, equivalent to `D:P(A;;GA;;;BA)(A;;GA;;;SY)`.
   - Keep the symbolic link only if the helper service needs it; otherwise document why it can be removed.

2. Replace the panic loop.
   - Declare `KeBugCheckEx`.
   - Use a WumaTracker-specific bugcheck code/tag.
   - Ensure the panic handler is divergent after calling `KeBugCheckEx`.

3. Re-check kernel stack usage.
   - Ensure the internal GWorld pattern scanner keeps large scan buffers in nonpaged pool, not on the kernel stack.
   - Ensure no user-mode input can choose scan patterns or widen scan ranges.
   - Keep remaining stack buffers small.

Verification:
- `rg -n "IoCreateDevice\\(|loop \\{\\}|panic_handler|KeBugCheckEx|D:P\\(A;;GA;;;BA\\)" driver/src/main.rs` shows secure creation and no panic loop.
- Driver Verifier smoke test reaches load/unload without immediate failure.
- A deliberate local panic test build produces the expected bugcheck signature, then the test-only panic trigger is removed.

## Milestone 4: Add the Helper Service

Goal: make the helper the only normal component that opens the driver.

Primary files:
- helper location chosen in Milestone 0
- `driver/shared/ioctls.rs`

1. Create the helper service project.
   - It must compile on Windows.
   - It must be installable as a Windows service or clearly documented as an elevated helper if service installation is deferred.
   - It must open `\\.\WumaDisplayService`.

2. Implement helper binary integrity checks.
   - Release builds must be Authenticode-signed.
   - At startup, verify the helper's own signature with `WinVerifyTrust`, or document an equivalent installer/service ACL guarantee.
   - Fail closed if the integrity check fails.

3. Implement a locked-down IPC endpoint.
   - Use a named pipe unless another Windows IPC choice is explicitly recorded.
   - Apply an explicit security descriptor.
   - Allow only the intended Tauri app identity and Administrators.
   - Do not grant Everyone or broad Authenticated Users access.

4. Implement the single operation.
   - Request: get current player location.
   - Helper action: call `IOCTL_GET_LOCATION`.
   - Response: coordinates or a typed error.
   - Do not implement raw IOCTL forwarding.
   - Do not accept PID, offsets, pointer chains, pattern bytes, base addresses, or target process names from IPC clients.

Verification:
- Helper build succeeds.
- Helper starts, opens the driver, and serves one get-location request.
- A non-authorized local client cannot connect to the pipe.
- `rg -n "DeviceIoControl|CreateFileW|WumaDisplayService" <helper-path>` shows driver access only inside the helper.

## Milestone 5: Move the Tauri App to Helper IPC

Goal: remove direct driver access from the app.

Primary file: `src-tauri/src/win_proc_driver.rs`.

1. Replace the direct driver handle with a helper IPC client.
   - Remove `DEVICE_PATH`.
   - Remove `open_device`.
   - Remove `auth`.
   - Remove direct `DeviceIoControl` use from this file.

2. Remove runtime driver configuration paths.
   - Remove `build_chain`.
   - Remove `SetConfigReq`.
   - Remove `PatternSearchReq`.
   - Remove `scan_gworld`.
   - Remove app-driven `rescan_gworld` behavior that triggers a driver pattern-scan IOCTL.

3. Simplify `WinProcDriver`.
   - Store helper connection state instead of a driver device handle and token.
   - Keep process liveness checks only if they are needed for UI state.
   - `get_player_info` should send exactly one helper request and convert the response into `PlayerInfo`.

4. Update user-facing errors.
   - Distinguish helper unavailable, helper unauthorized, driver unavailable, target process missing, and chain-read failure.

Verification:
- `rg -n "CreateFileW|DeviceIoControl|IOCTL_|SET_CONFIG|PATTERN_SEARCH|AUTH|WumaDisplayService" src-tauri/src/win_proc_driver.rs` returns no direct-driver protocol matches.
- `cargo check` in `src-tauri/` succeeds for the Windows target configuration.
- Running the app with helper stopped reports a helper-unavailable error.
- Running the app with helper running and game absent reports target-not-found.

## Milestone 6: Clean Shared Protocol and Build Scripts

Goal: prevent stale constants and packaging drift.

Primary files:
- `driver/shared/ioctls.rs`
- `driver/Cargo.toml`
- `driver/build.rs`
- `src-tauri/Cargo.toml`
- packaging or installer scripts

1. Ensure `driver/shared/ioctls.rs` is the only ABI definition.
   - Driver, helper, and app must import or generate from the same definitions where practical.
   - Remove copied stale IOCTL constants where possible.

2. Add driver metadata resources.
   - Product/component name.
   - Version.
   - Purpose description.
   - Publisher/project identity.

3. Update packaging.
   - Include the helper binary.
   - Include service installation or helper launch configuration.
   - Include signing steps for driver and helper.

Verification:
- `rg -n "0x800|0x801|0x804|222004|222010|IOCTL_SET_CONFIG|IOCTL_PATTERN_SEARCH"` returns no live code references.
- Release packaging produces driver, helper, and app artifacts.

## Milestone 7: Submission and Review Artifacts

Goal: make the security posture auditable without reading the full codebase.

Primary location: `driver/docs/`.

Create or update:
- `driver/docs/SUBMISSION.md`
- `driver/docs/TEST-MATRIX.md`
- `driver/docs/REVIEW-CHECKLIST.md`
- `driver/docs/PLAN-KO.md`

`SUBMISSION.md` must include:
- purpose statement
- architecture summary
- helper binary integrity explanation
- driver IOCTL list showing only `IOCTL_GET_LOCATION`
- device SDDL
- helper IPC security descriptor
- target process name
- maintenance note for game updates requiring a new signed driver build

`TEST-MATRIX.md` must include:
- Windows 10 22H2
- Windows 11 23H2
- Windows 11 24H2
- Driver Verifier load/unload
- target process absent
- helper absent
- unauthorized pipe client
- normal helper + game running path

`REVIEW-CHECKLIST.md` must include:
- no generic memory read/write
- no caller-supplied or generic pattern scan
- no runtime config
- no user-supplied PID
- no direct Tauri driver open
- object dereference audit
- purpose string found by `strings`

Verification:
- All three docs exist.
- `PLAN-KO.md` reflects the updated requirements-only structure.
- Submission docs do not contain real offset values.

## Final Acceptance Gate

The work is complete only when all checks pass:

- Driver exposes exactly one IOCTL: `IOCTL_GET_LOCATION`.
- Driver has no auth/config/caller-supplied pattern-scan protocol.
- Driver resolves only `Client-Win64-Shipping.exe`.
- Driver has no mutable global session state.
- Driver device is restricted to Administrators and SYSTEM.
- Driver panic path calls `KeBugCheckEx`.
- Helper is the only component that opens the driver in normal operation.
- Helper IPC is ACL-restricted and exposes only get-location.
- Tauri app has no direct driver IOCTL path.
- `cargo check` passes for changed Rust crates.
- Manual Windows QA covers helper absent, game absent, unauthorized pipe client, and normal game-running coordinate read.
- Submission docs exist and match the final implementation.

---

*End of TASKS.md*
