<#
.SYNOPSIS
    Builds the CAB submission package for Windows Hardware Dev Center
    Attestation Signing of the non-PnP WumaDisplayService driver.

.DESCRIPTION
    WumaDisplayService is a non-PnP kernel service driver (loaded via the
    Service Control Manager, see install-driver.ps1's `sc create` — there is
    no real PnP install path). Attestation Signing still requires an
    INF+CAT+driver bundle in a single signed CAB, so this script:

      1. Stages the already-built, already-Authenticode-signed .sys
         (run build-and-sign.ps1 first) together with its .pdb and the fake
         INF in driver/attestation/WumaDisplayService.inf. Per Microsoft's
         attestation-signing docs, the .pdb is a required part of the driver
         folder inside the CAB (not a separate upload) — it's what Microsoft's
         automated crash analysis tooling uses if this driver ever bugchecks
         on a user's machine.
      2. Runs InfVerif.exe against the staged INF and fails loudly on any
         error (per Microsoft's guidance, this is expected to be iterative —
         fix the INF and re-run until it's clean).
      3. Runs Inf2Cat.exe to produce WumaDisplayService.cat for the requested
         OS target list.
      4. Bundles .sys + .inf + .cat into WumaDisplayService.cab via the
         Windows-native makecab.exe.
      5. Signs the CAB itself with signtool, using the same cert flow as
         build-and-sign.ps1.

    The resulting signed CAB is what gets uploaded to the Partner Center
    Hardware Dashboard as an Attestation Signing submission — NOT a WHQL/HLK
    submission, so no .hlkx/.hckx test result package is produced or needed
    here.

    IMPORTANT: the certificate used to sign the CAB must be an EV code-signing
    certificate registered on the Partner Center Hardware Dashboard account.
    That may be a different certificate from the OV cert build-and-sign.ps1
    uses for day-to-day driver signing — verify before submitting.

.PARAMETER EwdkRoot
    Path to an extracted/mounted EWDK image (used to locate InfVerif.exe and
    Inf2Cat.exe).

.PARAMETER SysPath
    Path to the already-built, already-signed driver .sys. Defaults to
    build-and-sign.ps1's output location.

.PARAMETER PdbPath
    Path to the driver's .pdb. Defaults to cargo's release output location
    for the driver crate.

.PARAMETER OutDir
    Directory the staged INF/CAT/CAB are written to.

.PARAMETER OsList
    Comma-separated Inf2Cat /os: target list. Defaults to 10_X64 (Windows 10/11
    x64 client only, matching driver/docs/TEST-MATRIX.md's supported OS list).
    Do not add a ServerNNNN target: Windows Server 2016+ rejects
    attestation-signed device/filter drivers outright, so a Server target here
    would just make the submission ambiguous for no benefit.

.PARAMETER CertThumbprint
    SHA1 thumbprint of the EV code-signing certificate to sign the CAB with.
    Optional — when omitted, signtool is called with /a instead (auto-selects
    the best valid code-signing certificate from the CurrentUser\My store).

.PARAMETER TimestampUrl
    RFC 3161 timestamp server URL.

.EXAMPLE
    .\prepare-attestation-package.ps1

.EXAMPLE
    .\prepare-attestation-package.ps1 -EwdkRoot A:\EWDK -CertThumbprint <EV cert thumbprint>
#>
[CmdletBinding()]
param(
    [string]$EwdkRoot = "A:\EWDK",
    [string]$SysPath,
    [string]$PdbPath,
    [string]$OutDir,
    [string]$OsList = "10_X64",
    [string]$CertThumbprint,
    [string]$TimestampUrl = "http://time.certum.pl"
)

$ErrorActionPreference = "Stop"

$DriverDir = Split-Path -Parent $PSScriptRoot
$AttestationDir = Join-Path $DriverDir "attestation"
$InfSource = Join-Path $AttestationDir "WumaDisplayService.inf"

if (-not $SysPath) {
    $SysPath = Join-Path $DriverDir "target\WumaDisplayService.sys"
}
if (-not $PdbPath) {
    $PdbPath = Join-Path $DriverDir "target\x86_64-pc-windows-msvc\release\wuma_tracker_driver.pdb"
}
if (-not $OutDir) {
    $OutDir = Join-Path $DriverDir "target\attestation"
}

function Fail($msg) {
    Write-Error $msg
    exit 1
}

if (-not (Test-Path $SysPath)) {
    Fail "서명된 드라이버를 찾지 못했습니다: $SysPath (먼저 build-and-sign.ps1 실행)"
}
if (-not (Test-Path $PdbPath)) {
    Fail "심볼 파일을 찾지 못했습니다: $PdbPath (먼저 build-and-sign.ps1 실행)"
}
if (-not (Test-Path $InfSource)) {
    Fail "INF를 찾지 못했습니다: $InfSource"
}

# ── 0. 스테이징 ──────────────────────────────────────────────────────────────

Write-Host "[0/5] 스테이징 중: $OutDir" -ForegroundColor Cyan

$StageDir = Join-Path $OutDir "stage"
if (Test-Path $StageDir) {
    Remove-Item $StageDir -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

Copy-Item $SysPath (Join-Path $StageDir "WumaDisplayService.sys") -Force
Copy-Item $PdbPath (Join-Path $StageDir "WumaDisplayService.pdb") -Force
Copy-Item $InfSource (Join-Path $StageDir "WumaDisplayService.inf") -Force

# ── 1. 도구 탐색 (EWDK 버전에 안 묶이도록 트리에서 직접 찾는다) ──────────────────

Write-Host "[1/5] InfVerif.exe / Inf2Cat.exe 탐색 중..." -ForegroundColor Cyan

$toolsRoot = Join-Path $EwdkRoot "Program Files\Windows Kits\10\Tools"
$binRoot = Join-Path $EwdkRoot "Program Files\Windows Kits\10\bin"

$infVerif = Get-ChildItem $toolsRoot -Recurse -Filter "infverif.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "\\x64\\" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $infVerif) {
    Fail "infverif.exe를 찾지 못했습니다: $toolsRoot"
}

$inf2cat = Get-ChildItem $binRoot -Recurse -Filter "inf2cat.exe" -ErrorAction SilentlyContinue |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $inf2cat) {
    Fail "inf2cat.exe를 찾지 못했습니다: $binRoot"
}

# ── 2. InfVerif ──────────────────────────────────────────────────────────────
# 실패하면 스크립트를 고치지 말고 WumaDisplayService.inf를 고쳐서 재실행할 것.
# (OSR/Microsoft 가이드 모두 "에러 없어질 때까지 반복 수정"이 정상 절차라고 명시)

Write-Host "[2/5] InfVerif.exe 검증 중..." -ForegroundColor Cyan

& $infVerif /v /w (Join-Path $StageDir "WumaDisplayService.inf")
if ($LASTEXITCODE -ne 0) {
    Fail "InfVerif 실패 (exit $LASTEXITCODE). WumaDisplayService.inf를 수정한 뒤 다시 실행하세요."
}

# ── 3. Inf2Cat ───────────────────────────────────────────────────────────────

Write-Host "[3/5] Inf2Cat.exe로 카탈로그 생성 중 (/os:$OsList)..." -ForegroundColor Cyan

& $inf2cat /driver:$StageDir /os:$OsList /verbose
if ($LASTEXITCODE -ne 0) {
    Fail "Inf2Cat 실패 (exit $LASTEXITCODE)"
}

$catFile = Join-Path $StageDir "WumaDisplayService.cat"
if (-not (Test-Path $catFile)) {
    Fail "카탈로그 파일이 생성되지 않았습니다: $catFile"
}

# ── 4. CAB 패키징 (makecab, Windows 내장 도구) ────────────────────────────────

Write-Host "[4/5] CAB 패키징 중..." -ForegroundColor Cyan

$cabPath = Join-Path $OutDir "WumaDisplayService.cab"
$ddfPath = Join-Path $OutDir "WumaDisplayService.ddf"

# Partner Center's package validation rejects a CAB with files directly at
# its root ("There are files at the root of the cabinet: ...") — driver
# package files must be isolated under a subdirectory. DestinationDir nests
# everything under WumaDisplayService\ inside the CAB.
@"
.OPTION EXPLICIT
.Set CabinetNameTemplate=WumaDisplayService.cab
.Set DiskDirectory1=$OutDir
.Set Cabinet=on
.Set Compress=on
.Set DestinationDir=WumaDisplayService
"$StageDir\WumaDisplayService.sys"
"$StageDir\WumaDisplayService.pdb"
"$StageDir\WumaDisplayService.inf"
"$StageDir\WumaDisplayService.cat"
"@ | Set-Content -Path $ddfPath -Encoding ASCII

if (Test-Path $cabPath) {
    Remove-Item $cabPath -Force
}

& makecab.exe /F $ddfPath | Out-Null
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $cabPath)) {
    Fail "makecab 실패 (exit $LASTEXITCODE)"
}

# ── 5. CAB 서명 (Partner Center 계정에 등록된 EV 인증서여야 함) ────────────────

Write-Host "[5/5] signtool로 CAB 서명 중..." -ForegroundColor Cyan

$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter "signtool.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "\\x64\\" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
if (-not $signtool) {
    Fail "signtool.exe를 찾지 못했습니다."
}

$certSelectArgs = if ($CertThumbprint) { @("/sha1", $CertThumbprint) } else { @("/a") }

$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$signArgs = @("sign") + $certSelectArgs + @("/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256", "/v", $cabPath)
& $signtool @signArgs
if ($LASTEXITCODE -ne 0) {
    $ErrorActionPreference = $prevEap
    Fail "CAB 서명 실패 (exit $LASTEXITCODE)"
}
$ErrorActionPreference = $prevEap

Write-Host "Attestation 제출 패키지 준비 완료: $cabPath" -ForegroundColor Green
Write-Host "Partner Center Hardware Dashboard에서 제출 타입을 'Attestation Signing'으로 선택하고 이 CAB을 업로드하세요 (WHQL 아님, .hlkx 불필요)." -ForegroundColor Green
