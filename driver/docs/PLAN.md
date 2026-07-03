---
title: WumaTracker Driver Security Requirements
status: canonical
location: driver/docs/PLAN.md
merged-from:
  - .junie/plans/helper-service-security-plan.md
  - .junie/plans/enhance-helper-security-plan.md
last-updated: 2026-07-03
---

# WumaTracker Driver Security Requirements

## Purpose

`PLAN.md` is the requirements source of truth. It states the final security properties the driver system must satisfy. It does not describe implementation order, temporary migration states, or code-level task breakdowns. Concrete execution steps live in `driver/docs/TASKS.md`.

The final system must make the WumaTracker Windows driver a single-purpose player-position telemetry component. It must not be usable as a generic memory reader, pattern scanner, arbitrary pointer-chain walker, or privilege escalation primitive.

## Scope

In scope:
- Windows kernel driver hardening for the WumaTracker coordinate-reading path.
- A privileged Windows helper service that mediates all app access to the driver.
- The Tauri app integration with that helper service.
- Submission and review artifacts needed to explain the driver's legitimate single purpose.
- Documentation under `driver/docs/`.

Out of scope:
- WDF migration. WDF remains a future maintainability option, not a requirement for this hardening pass.
- Publishing real game offset values in documentation.
- Implementing general process inspection, memory read/write, debugging, scanning, or anti-cheat bypass features.

## Threat Model

The design must reduce these risks:
- Any local process opening the device object directly.
- A tampered Tauri app using the driver outside the intended overlay workflow.
- A compromised helper using the driver against an arbitrary process.
- Reusing the driver as a vulnerable-driver primitive by passing arbitrary PID, base address, chain, or pattern parameters.
- Static scanners or human reviewers classifying the binary as a generic game-cheat or LOLDriver-style read primitive.
- Kernel instability being hidden by a permanent panic loop.

The design does not claim to protect against:
- A fully compromised kernel.
- A stolen or misissued signing certificate.
- Game updates that change the executable name, module layout, or coordinate chain.

## Final Architecture Requirements

The final architecture has three components:
- **Driver**: A single-purpose kernel component that reads only the current player's world position from the hardcoded target process and hardcoded coordinate chain.
- **Helper service**: A privileged, signed Windows service or elevated process that owns the driver handle and exposes a restricted local IPC contract.
- **Tauri app**: A normal app process that requests position data only through the helper service.

The Tauri app must not open `\\.\WumaDisplayService` directly in the final architecture.

## Driver Requirements

The driver must expose exactly one IOCTL in the final state: `IOCTL_GET_LOCATION`.

The driver must not expose or retain:
- `IOCTL_AUTH`
- `IOCTL_SET_CONFIG`
- `IOCTL_PATTERN_SEARCH`
- session tokens
- caller-supplied PID authentication
- caller-supplied base addresses
- caller-supplied pointer chains
- caller-supplied pattern bytes or scan ranges
- mutable global session configuration

The driver must hardcode the target process name as `Client-Win64-Shipping.exe`, matching the Windows game process name used by the app.

The driver must resolve the target process internally. It must not accept a PID from user mode. The process lookup method must support the full target image name; if an API only exposes truncated image names, that API is not sufficient by itself.

The driver must hardcode the coordinate chain needed to read player world position:
- game module base resolution strategy
- GWorld anchor strategy
- player transform chain
- world-origin chain
- transform and origin offsets

The driver must return coordinates only for this hardcoded chain. It must not provide a fallback path that reads arbitrary memory or accepts runtime replacement offsets.

All referenced kernel process objects must be dereferenced on every success and failure path. No referenced `PEPROCESS` may be returned to user mode.

The device object must be created with restrictive security. Administrators and SYSTEM may access the device; ordinary users and arbitrary local processes must not.

The panic handler must fail explicitly with `KeBugCheckEx` using a WumaTracker-specific code/tag. It must not loop forever.

The driver binary must include durable purpose metadata that survives normal release stripping and is discoverable with standard binary inspection tools. The metadata must clearly state that the driver only reads player world coordinates for the legitimate WumaTracker overlay service.

## Helper Service Requirements

The helper must run as a Windows service or elevated helper process installed with restrictive filesystem and service ACLs.

The helper binary must be Authenticode-signed with the project's production signing certificate for release builds. EV signing is required for release distribution where available, but signing alone is not the driver trust boundary.

The helper must verify its own release signature at startup, or the installer/service configuration must enforce an equivalent binary integrity guarantee. The chosen guarantee must be documented in the submission package.

The helper must be the only component that opens the driver device in normal operation.

The helper must expose a local IPC endpoint using a named pipe or an equivalent Windows IPC mechanism with an explicit security descriptor.

The helper IPC ACL must allow only the intended local app identity and administrators. It must not allow broad access such as Everyone, Authenticated Users, or unrestricted local user access.

The helper IPC contract must expose only one operation: get current player location. It must not expose raw IOCTL forwarding, generic memory read/write, pattern scanning, configurable pointer chains, or PID selection.

The helper must return explicit error states for:
- helper not authorized or signature check failed
- driver device unavailable
- target process not found
- coordinate chain read failure
- driver returned an unexpected response

## Tauri App Requirements

The Tauri app must communicate with the helper service instead of opening the driver device directly.

The app must not contain final-state code paths that call driver IOCTLs directly.

The app may continue to use user-mode process checks for launch state and user-facing status, but those checks must not be used to configure kernel memory reads.

The app must surface helper and driver failures as actionable user-facing errors without falling back to arbitrary user-mode memory reads through the driver.

## Purpose Metadata Requirements

All new driver-facing identifiers, comments, binary metadata, and submission text must use English first. A concise Korean sentence may be included for local reviewers.

The binary metadata must include:
- product/component name
- single-purpose position telemetry description
- explicit "no write, no pattern scan, no generic memory access, no runtime configuration" language
- project or publisher identity
- version information

If Rust string literals are used for metadata, UTF-8 text must be represented in a Rust-valid way. Korean text must not be placed directly inside a `b"..."` byte string unless encoded with explicit byte escapes.

## Review Package Requirements

The submission package must include:
- purpose statement
- architecture summary
- helper service trust and binary integrity explanation
- driver IOCTL list showing only `IOCTL_GET_LOCATION`
- device SDDL or equivalent security descriptor
- helper IPC security descriptor
- target process name and rationale
- explanation that coordinate-chain updates require a new signed driver build
- test matrix for Windows 10 22H2 and Windows 11 23H2/24H2
- Driver Verifier and HLK notes
- `strings` or equivalent evidence showing purpose metadata in the binary

## Non-Functional Requirements

The final implementation must remain x64 Windows 10+ only unless a separate compatibility plan is approved.

The driver must avoid large kernel stack allocations. Buffers larger than approximately 1 KB must use nonpaged pool or another appropriate kernel allocation strategy.

All kernel error paths must be explicit and diagnosable.

The final ABI must be documented in one shared source of truth so the driver, helper, and app cannot drift.

The design accepts the maintenance cost that game updates may require a new signed driver build.

## Canonical Documents

`driver/docs/PLAN.md` defines requirements.

`driver/docs/TASKS.md` defines implementation order, file-level changes, and verification.

`driver/docs/PLAN-KO.md` is the Korean translation and must be updated when the English requirements materially change.

---

*End of English PLAN.md*
