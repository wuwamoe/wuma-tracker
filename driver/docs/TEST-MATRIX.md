---
title: WumaTracker Driver Test Matrix
status: canonical
location: driver/docs/TEST-MATRIX.md
based-on: driver/docs/TASKS.md
last-updated: 2026-07-03
---

# WumaTracker Driver Test Matrix

Manual QA required before release. `cargo check` (driver, helper, src-tauri)
must also pass; see `driver/scripts/ewdk-check.ps1` for the driver's EWDK
check and plain `cargo check` for the helper and app crates.

| # | Scenario | OS target | Expected result | Status |
|---|----------|-----------|------------------|--------|
| 1 | Driver load/unload | Windows 10 22H2 | Loads cleanly, symbolic link created, unloads cleanly | Pending manual QA |
| 2 | Driver load/unload | Windows 11 23H2 | Loads cleanly, symbolic link created, unloads cleanly | Pending manual QA |
| 3 | Driver load/unload | Windows 11 24H2 | Loads cleanly, symbolic link created, unloads cleanly | Pending manual QA |
| 4 | Driver Verifier load/unload | Windows 11 24H2 (or latest supported) | Loads and unloads under Driver Verifier without a bugcheck | Pending manual QA |
| 5 | Target process absent | Any supported OS | Helper's `IOCTL_GET_LOCATION` reports target-not-found (driver returns `STATUS_NOT_FOUND`; helper surfaces stage `0xFE`); app shows `target_process_missing` | Pending manual QA |
| 6 | Helper absent | Any supported OS | App's pipe connect fails; app shows `helper_unavailable` | Pending manual QA |
| 7 | Unauthorized pipe client | Any supported OS | A process running as a different, non-Administrator account cannot open `\\.\pipe\WumaTrackerHelper` (`ERROR_ACCESS_DENIED`) | Pending manual QA |
| 8 | Normal helper + game running | Any supported OS | App receives valid coordinates end to end (app -> helper -> driver -> game process) | Pending manual QA |
| 9 | Driver device opened directly by an unprivileged process | Any supported OS | `CreateFileW(\\.\WumaDisplayService)` from a non-Administrator, non-SYSTEM process fails with access denied | Pending manual QA |
| 10 | Helper self-signature check (release build) | Any supported OS | Running an unsigned or tampered helper binary refuses to start | Pending manual QA |

Update the Status column as each scenario is executed; do not mark the
milestone or release complete while any row is `Pending manual QA` for a
release build.
