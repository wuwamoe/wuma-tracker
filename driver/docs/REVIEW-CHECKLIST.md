---
title: WumaTracker Driver Review Checklist
status: canonical
location: driver/docs/REVIEW-CHECKLIST.md
based-on: driver/docs/TASKS.md, driver/docs/PLAN.md
last-updated: 2026-07-03
---

# WumaTracker Driver Review Checklist

Use this checklist when reviewing a change to `driver/`, `driver/helper/`, or
`src-tauri/src/win_proc_driver.rs`. Every item must hold for the change to be
mergeable.

- [ ] **No generic memory read/write.** `driver/src/main.rs` has no IOCTL or
      internal path that reads or writes an address supplied by a caller.
      `kread`/`kread_ptr` are only ever called with addresses derived from
      `PLAYER_COORD_CONFIG` or the internally-discovered world anchor.
- [ ] **No caller-supplied or generic pattern scan.** `find_hardcoded_world_anchor`
      only searches for `WORLD_ANCHOR_PATTERN_PREFIX`/`WORLD_ANCHOR_PATTERN_SUFFIX`, both
      compile-time constants. No IOCTL accepts prefix/suffix bytes, a scan
      range, or a target process from a caller.
- [ ] **No runtime config.** There is no mutable global session/config state
      (no `SESSION_TOKEN`, `AUTHED_PID`, `CONFIG_READY`, `static mut CONFIG`).
      `PLAYER_COORD_CONFIG` is an immutable `static`.
- [ ] **No user-supplied PID.** `find_target_process` enumerates processes
      internally via `PsGetNextProcess` and matches by hardcoded image name;
      no IOCTL request carries a PID.
- [ ] **No direct Tauri driver open.** `src-tauri/src/win_proc_driver.rs` never
      calls `CreateFileW` on `\\.\WumaDisplayService`; it only opens
      `\\.\pipe\WumaTrackerHelper`. Verify with:
      `rg -n "WumaDisplayService" src-tauri/src/win_proc_driver.rs` — the only
      match should be an explanatory comment, not a `CreateFileW` call.
- [ ] **Object dereference audit.** Every `PEPROCESS` obtained from
      `PsGetNextProcess` or returned by `find_target_process` is dereferenced
      via `ObDereferenceObjectDeferDelete` on every success and failure path.
      No `PEPROCESS` is ever copied into a response buffer or otherwise
      exposed to user mode.
- [ ] **Purpose string found by `strings`.** After a release build,
      `strings target\...\WumaDisplayService.sys | grep "WumaTracker Player Position Service"`
      finds the embedded `WUMATRACKER_PURPOSE` string.
- [ ] **Single ABI source.** `driver/shared/ioctls.rs` is the only place
      `IOCTL_GET_LOCATION`, `DEVICE_NAME`, and `TARGET_PROCESS_NAME` are
      defined; the driver and helper both `#[path]`-include it rather than
      redefining the constants.
- [ ] **Device and pipe ACLs unchanged or reviewed.** Any change to the
      device SDDL (`D:P(A;;GA;;;BA)(A;;GA;;;SY)`) or the pipe's dynamic SDDL
      in `driver/helper/src/main.rs` is deliberate and documented in
      `driver/docs/SUBMISSION.md`.
- [ ] **`cargo check` passes** for `driver/` (via
      `driver/scripts/ewdk-check.ps1` or `build-and-sign.ps1 -SkipSign`),
      `driver/helper/`, and `src-tauri/`.
