---
title: WumaTracker Driver — Helper Removal / Direct-Access Requirements
status: canonical
location: driver/docs/DIRECT-ACCESS-PLAN.md
supersedes:
  - driver/docs/PLAN.md ("Final Architecture Requirements", "Helper Service Requirements")
last-updated: 2026-08-18
---

# WumaTracker Driver — Helper Removal / Direct-Access Requirements

## Decision

Remove the helper service. The Tauri app opens `\\.\WumaDisplayService`
directly and calls `IOCTL_GET_LOCATION` itself. The device SDDL
(Administrators/SYSTEM only) remains the actual trust boundary — no separate
privileged process is needed to hold it. The driver adds cheap,
non-cryptographic friction at handle-open time (image name check,
single-instance handle, PID binding) to discourage incidental misuse by other
admin-level processes, but does not attempt to cryptographically authenticate
the caller — see "Threat model changes" for why that goal was explored and
dropped.

## Rationale

- The app already runs elevated (Administrator) for its normal operation —
  there is no current requirement to run it as a standard user, so the
  helper's original reason for existing (let a privileged component own the
  device handle so the app itself doesn't need elevation) doesn't apply today.
- A separate helper process adds real, ongoing cost for no benefit under that
  condition: a second binary to build/sign/ship, a service/process lifecycle
  to manage (start, crash-recovery, version skew between app and helper), and
  an extra IPC hop (named pipe) with its own ACL and failure modes.
- If a future requirement forces the app to drop elevation, the helper
  pattern (or an equivalent broker) becomes necessary again — this is an
  explicit trade accepted now, not a permanent architectural stance. See
  "Reversal condition" below.

## Threat model changes

Kept from `driver/docs/PLAN.md`'s threat model, unchanged:
- Any local process opening the device object directly — still mitigated by
  the device SDDL (Administrators/SYSTEM only).
- Reusing the driver as a vulnerable-driver (BYOVD) primitive via
  caller-supplied PID/address/chain/pattern — still mitigated by the driver
  accepting zero input on `IOCTL_GET_LOCATION`.
- Kernel instability hidden by a permanent panic loop — unchanged
  (`KeBugCheckEx`).

New risk introduced by removing the helper:
- **Any other Administrator/SYSTEM-level process on the machine could
  previously piggyback on a running helper's already-open handle indirectly
  only through the pipe** (blocked by the pipe's own ACL); with no helper,
  *any* admin-level process can call `CreateFileW` on the device directly,
  same as before the helper existed. The device SDDL alone does not
  distinguish "our app" from "some other admin-level program."

**Explored and rejected as security boundaries** (all researched and
discussed before landing on the design below — kept here so this isn't
re-litigated):
- `SeGetCachedSigningLevel` — not a documented, supported WDK API. No
  Microsoft Learn reference page; public use is concentrated in security
  research for locating CI callback pointers, not sanctioned third-party
  driver code. Depending on it risks both ABI instability and Microsoft's
  attestation-submission scanning flagging an undocumented-internal-export
  pattern otherwise associated with cheat/malware tooling.
- Full image path comparison as a *security* claim — rejected: any caller
  who is already Administrator can write to the expected install path and
  defeat it, so it doesn't hold up as an actual boundary. (It survives below,
  demoted to a non-security "friction" check — see that section.)
- Challenge-response with a TPM-backed non-exportable key
  (`NCrypt`/`Microsoft Platform Crypto Provider`, verified in-kernel via
  documented `BCryptVerifySignature`/`cng.lib`) — technically sound and fully
  documented-API-based, but rejected for two concrete reasons: it requires a
  TPM (not universally present), and registering the public key with the
  driver at runtime is inherently a trust-on-first-use race (whichever
  process registers first after driver load becomes "trusted" for that
  session) — not an acceptable amount of nondeterminism for what it costs to
  build.
- Software-backed (non-TPM) persisted keys, DPAPI-protected secrets, or an
  embedded plaintext key — all rejected on the same basis: every one of these
  is either readable, or usable-via-the-same-API, by any other process
  running as the same account, which is exactly the threat being defended
  against. Without a hardware root of trust, no locally-stored secret is safe
  from an equally-privileged co-resident process — this is a general limit,
  not something specific to this project.

**Conclusion on identity verification:** there is no available mechanism —
documented or not, hardware-backed or not — that actually proves "this
caller is our signed app" against a same-account, admin-level co-resident
process, short of a TPM-bound scheme this project has already rejected on
practicality grounds. Given that a caller who successfully defeats any
identity check here is, by construction, already at a privilege level where
loading their own kernel driver is strictly easier than defeating this one's
narrow IOCTL, the design below stops trying to *authenticate* the caller and
instead (a) minimizes what a successful-but-unintended caller can get, and
(b) adds cheap friction against casual/accidental misuse. This matches how
real anti-cheat kernel drivers approach this in practice (ACL restriction +
session/handle binding — not per-caller cryptographic identity checks; see
research notes below).

## Driver requirements

### Handle-open friction (not a security boundary)

At `IRP_MJ_CREATE`, the driver checks the calling process's image name via
`IoGetCurrentProcess()` (the actual ntoskrnl export; `PsGetCurrentProcess` is
just its wdm.h macro alias, hence wdk_sys binds the former) — valid here
because `IRP_MJ_CREATE` runs synchronously in the caller's own thread context
— and the same truncated-name comparison technique already used in
`try_cached_process`'s `CACHED_NAME_PREFIX` check —
compare against the expected app binary name (`wuma-tracker.exe`, matching
`mainBinaryName` in `src-tauri/tauri.conf.json`). Reject
(`STATUS_ACCESS_DENIED`) on mismatch.

This is explicitly **not** presented as identity authentication anywhere —
it costs nothing, reuses an existing pattern, and is trivially defeated by
renaming any binary to match. Its only purpose is to stop a *different*
program from casually/accidentally opening the device without deliberately
choosing to impersonate this one's name — friction against incidental
misuse, not a defense against a targeted attacker. Document it as such
wherever it's mentioned (code comments, submission docs) so it's never later
mistaken for a real access control layer.

### Single-instance device handle

The device rejects a second concurrent `IRP_MJ_CREATE` while one handle is
already open (same idea as the helper's old
`FILE_FLAG_FIRST_PIPE_INSTANCE`-style named pipe). While the app is running
and holding its handle open, no other process — regardless of admin status —
can open a second, independent handle to the device. This doesn't verify
identity, but it meaningfully shrinks the exposure window: a would-be
opportunistic caller can only get in while the legitimate app isn't running
at all, not "any time alongside it."

### Handle/PID binding

The PID that successfully opens the device at `IRP_MJ_CREATE` is recorded
(e.g., in the `FILE_OBJECT`'s `FsContext`). Every subsequent
`IOCTL_GET_LOCATION` on that handle re-checks the calling PID against the
recorded one, rejecting on mismatch. This defends specifically against
handle-duplication/inheritance tricks (e.g. a more-privileged process
duplicating the legitimate handle into another process) — it does not by
itself stop a separate process from opening its own fresh handle; that's what
single-instance (above) is for.

### Not mitigated (explicitly accepted)

Consistent with the existing `PLAN.md` non-goals: a fully compromised kernel,
or an attacker capable of loading their own signed/vulnerable driver. At that
point they have stronger primitives available than this driver's single
narrow read-only IOCTL, so none of the mechanisms above are expected to (or
need to) hold against that tier of attacker.

### Research note

A review of public material on kernel-mode anti-cheat drivers (Vanguard,
BattlEye, EasyAntiCheat-class systems) did not surface any publicly
documented use of per-caller cryptographic identity verification for
device-open access control. The consistently-cited real pattern is ACL
restriction on the device object plus session/PID binding in the dispatch
routine — which is exactly the design landed on above, arrived at
independently before this was confirmed via research.

### Device object / IOCTL surface (unchanged)

- Device SDDL stays `D:P(A;;GA;;;BA)(A;;GA;;;SY)` — Administrators/SYSTEM
  only. This remains the real access-control gate; the name check,
  single-instance restriction, and PID binding above are all supplementary
  and none of them replace it.
- `IOCTL_GET_LOCATION` remains the only IOCTL, still accepts zero input bytes.
  No new IOCTL is introduced by this change.

### Removed from the driver

This plan itself does not change the driver's coordinate-reading path — the
`PLAYER_COORD_CONFIG` offset fix (v3.4.0+ chain, `FTransform`/quaternion read,
world-origin rebasing) and the stage-1 cache-invalidation fix were separate,
prior work in the same session, not part of implementing this plan. This plan
only adds `IRP_MJ_CREATE`/`IRP_MJ_CLOSE` handling (name check, single-instance
enforcement, PID recording) and a PID check in the `IOCTL_GET_LOCATION`
dispatch path; it does not touch `read_player_world_position`,
`PLAYER_COORD_CONFIG`, or process/anchor resolution.

## App requirements

- `src-tauri/src/win_proc_driver.rs` (or a renamed equivalent) opens
  `\\.\WumaDisplayService` directly via `CreateFileW` and calls
  `IOCTL_GET_LOCATION` via `DeviceIoControl`, replacing the current
  named-pipe call to the helper.
- The app continues to require Administrator elevation to run at all (no
  change to its current launch requirements) — this is what makes removing
  the helper safe under today's requirements; if that ever changes, re-read
  "Reversal condition" below before proceeding.
- App-facing error states collapse from the current
  `HelperUnavailable`/`HelperUnauthorized`/`DriverUnavailable` set to just:
  driver device unavailable (open failed — covers "not loaded" and "caller
  check rejected" as the same user-facing state, since the app cannot itself
  distinguish "driver not present" from "driver present but rejected me" from
  the `CreateFileW` failure alone, and the difference isn't user-actionable
  anyway), target process not found, coordinate chain read failure.

### Removed from the app/repo

- `driver/helper/` (entire crate).
- The named pipe client code in `win_proc_driver.rs` (`open_helper_pipe`,
  `PIPE_NAME`, the `REQ_GET_LOCATION` wire opcode — the app now issues the
  IOCTL directly instead of relaying through a pipe protocol).
- `driver/scripts/test-helper-pipe.ps1` (no helper pipe left to test against;
  a direct-IOCTL equivalent may replace it if a manual test tool is still
  wanted).
- The helper build/sign/package steps in `driver/scripts/build-and-sign.ps1`
  (currently step `[0/6]`) and the helper binary references in
  `src-tauri/windows/wix7/main.wxs` (the `HelperExe` component and its
  service-install custom actions — those become unnecessary once nothing
  installs or runs a helper service).

## Reversal condition

If a future requirement forces the Tauri app to run as a standard (non-admin)
user, this plan's premise no longer holds, and a broker process (helper, or
an equivalent OS service) becomes necessary again to own the elevated device
handle. That is a re-adoption of the old architecture, not a new design — do
not attempt to make the *driver* solve "let a non-admin caller in" by
weakening the device SDDL; the SDDL staying Administrators/SYSTEM-only is not
up for negotiation under either architecture.

## Non-goals

Unchanged from `driver/docs/PLAN.md`: no generic memory read/write, no
caller-supplied pattern scan, no runtime configuration, no user-supplied PID.
This plan does not reopen any of those.

## Documentation follow-up (not part of this spec, tracked separately)

Once implemented, `driver/docs/PLAN.md`, `TASKS.md`, `SUBMISSION.md`,
`REVIEW-CHECKLIST.md`, and `TEST-MATRIX.md` all still describe the
three-component (app/helper/driver) architecture and need to be brought in
line with this two-component design. Not done as part of writing this spec.

---

*End of DIRECT-ACCESS-PLAN.md*
