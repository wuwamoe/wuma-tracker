<#
.SYNOPSIS
    Builds the WumaTracker kernel driver with an Enterprise WDK (EWDK) and code-signs it.

.DESCRIPTION
    This machine does not have a full WDK installed (only the plain Windows SDK), so the
    driver must be built from an EWDK image (see https://learn.microsoft.com/windows-hardware/drivers/download-the-wdk).
    EWDK's own SetupBuildEnv.cmd calls vsdevcmd with -winsdk=none, which leaves $env:LIB
    without the SDK/CRT/MSVC library paths needed to link a host build-script binary
    (wdk-sys's build.rs). This script re-derives those paths from the EWDK tree itself
    instead of hardcoding a toolset version, so it keeps working across EWDK updates.

.PARAMETER EwdkRoot
    Path to an extracted/mounted EWDK image (contains LaunchBuildEnv.cmd).

.PARAMETER CertThumbprint
    SHA1 thumbprint of a specific code-signing certificate in the current user's
    certificate store (CurrentUser\My). Optional — when omitted, signtool is
    called with /a instead, which auto-selects the best valid code-signing
    certificate from the store (this is what plain `signtool sign /a ...`
    does).

.PARAMETER TimestampUrl
    RFC 3161 timestamp server URL. Certum's is used by default since that's the CA in use.

.PARAMETER SkipSign
    Build only; skip the signtool step (useful for quick compile checks).

.EXAMPLE
    .\build-and-sign.ps1

.EXAMPLE
    .\build-and-sign.ps1 -CertThumbprint B9487CB38D51E2201079B302963AC0E41EE1B933

.EXAMPLE
    .\build-and-sign.ps1 -EwdkRoot A:\EWDK -SkipSign
#>
[CmdletBinding()]
param(
    [string]$EwdkRoot = "A:\EWDK",
    [string]$CertThumbprint,
    [string]$TimestampUrl = "http://time.certum.pl",
    [switch]$SkipSign,
    [string]$Arch = "amd64"
)

$ErrorActionPreference = "Stop"

$DriverDir = Split-Path -Parent $PSScriptRoot
$HelperDir = Join-Path $DriverDir "helper"
$OutSys = Join-Path $DriverDir "target\WumaDisplayService.sys"
$OutHelperExe = Join-Path $DriverDir "target\wuma_tracker_helper.exe"

function Fail($msg) {
    Write-Error $msg
    exit 1
}

$setupBuildEnv = Join-Path $EwdkRoot "BuildEnv\SetupBuildEnv.cmd"
if (-not (Test-Path $setupBuildEnv)) {
    Fail "EWDK를 찾을 수 없습니다: $setupBuildEnv (-EwdkRoot 확인)"
}

# ── 0. 헬퍼 빌드 (일반 win32 바이너리, EWDK의 winsdk=none 환경이 아니라 시스템에
#      설치된 일반 MSVC/Windows SDK로 빌드해야 하므로 EWDK 환경 로드 전에 먼저 한다) ──

Write-Host "[0/6] 헬퍼 빌드 중 (cargo build --release, $HelperDir)..." -ForegroundColor Cyan

Push-Location $HelperDir
try {
    # driver/.cargo/config.toml's target.<triple>.rustflags (kernel-driver
    # linker flags: /DRIVER /SUBSYSTEM:NATIVE /NODEFAULTLIB /ENTRY:DriverEntry
    # /INTEGRITYCHECK, static CRT) leaks in here because cargo config discovery
    # walks up the directory tree and MERGES (appends) array-typed keys like
    # rustflags across every config file it finds along the way — a closer
    # driver\helper\.cargo\config.toml, or even a --config CLI override for
    # the same key, only adds to that list rather than replacing it. Setting
    # the real RUSTFLAGS environment variable is the one thing that actually
    # wins outright over config-file rustflags instead of merging with them,
    # so it's used here (scoped to this one invocation) to build a normal
    # usermode binary instead of a kernel driver.
    $prevRustflags = $env:RUSTFLAGS
    # A literal "" is indistinguishable from unset on Windows (assigning it
    # removes the variable from the child process environment instead of
    # keeping it defined-but-empty), which would silently fall back to the
    # merged config rustflags again. A single space is a real, non-empty
    # value that rustc's whitespace-based flag splitting treats as no flags.
    $env:RUSTFLAGS = " "
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) {
            Fail "헬퍼 cargo build 실패 (exit $LASTEXITCODE)"
        }
    } finally {
        $env:RUSTFLAGS = $prevRustflags
    }
} finally {
    Pop-Location
}

$builtHelperExe = Join-Path $HelperDir "target\x86_64-pc-windows-msvc\release\wuma_tracker_helper.exe"
if (-not (Test-Path $builtHelperExe)) {
    $builtHelperExe = Join-Path $HelperDir "target\release\wuma_tracker_helper.exe"
}
if (-not (Test-Path $builtHelperExe)) {
    Fail "헬퍼 빌드 산출물을 찾지 못했습니다: $HelperDir\target\{x86_64-pc-windows-msvc\release,release}\wuma_tracker_helper.exe"
}
New-Item -ItemType Directory -Force -Path (Split-Path $OutHelperExe) | Out-Null
Copy-Item $builtHelperExe $OutHelperExe -Force

# ── 1. EWDK 빌드 환경 로드 (cmd에서 실행 후 env를 PowerShell로 가져옴) ──────────

Write-Host "[1/6] EWDK 빌드 환경 로드 중 ($EwdkRoot, $Arch)..." -ForegroundColor Cyan

$envDumpFile = New-TemporaryFile
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
cmd /c "`"$setupBuildEnv`" $Arch && set" > $envDumpFile.FullName 2>&1
$ErrorActionPreference = $prevEap

$envVars = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
Get-Content $envDumpFile.FullName | ForEach-Object {
    if ($_ -match '^([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
        $envVars[$matches[1]] = $matches[2]
    }
}
Remove-Item $envDumpFile.FullName -ErrorAction SilentlyContinue

if (-not $envVars.ContainsKey("WDKContentRoot") -or -not $envVars.ContainsKey("Version_Number")) {
    Fail "EWDK 환경 변수(WDKContentRoot/Version_Number)를 읽지 못했습니다. SetupBuildEnv.cmd 출력을 확인하세요."
}

foreach ($key in $envVars.Keys) {
    if ($key -ieq "Path" -and $envVars.ContainsKey("PATH")) {
        continue
    }
    Set-Item -Path "env:$key" -Value $envVars[$key]
}
if ($envVars.ContainsKey("PATH")) {
    Set-Item -Path "env:Path" -Value $envVars["PATH"]
}

$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"

# ── 2. LIB 경로 보강 ─────────────────────────────────────────────────────────
# vsdevcmd -winsdk=none 때문에 빠진 um/ucrt/MSVC lib 경로를 EWDK 트리에서 직접 찾아 채운다.

Write-Host "[2/6] 링커 LIB 경로 보강 중..." -ForegroundColor Cyan

$sdkVersion = $envVars["Version_Number"]
$sdkRoot = $envVars["WDKContentRoot"].TrimEnd('\')

$msvcToolsRoot = Join-Path $EwdkRoot "Program Files\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC"
$msvcVersionDir = Get-ChildItem $msvcToolsRoot -Directory | Sort-Object Name -Descending | Select-Object -First 1
if (-not $msvcVersionDir) {
    Fail "MSVC 툴셋을 찾지 못했습니다: $msvcToolsRoot"
}

$extraLib = @(
    (Join-Path $sdkRoot "Lib\$sdkVersion\um\x64"),
    (Join-Path $sdkRoot "Lib\$sdkVersion\ucrt\x64"),
    (Join-Path $msvcVersionDir.FullName "lib\x64")
) -join ";"

$env:LIB = "$($env:LIB);$extraLib"

# ── 3. 빌드 ──────────────────────────────────────────────────────────────────

Write-Host "[3/6] cargo build --release ($DriverDir)..." -ForegroundColor Cyan

Push-Location $DriverDir
try {
    & cargo build --release
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo build 실패 (exit $LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

$builtExe = Join-Path $DriverDir "target\x86_64-pc-windows-msvc\release\wuma_tracker_driver.exe"
if (-not (Test-Path $builtExe)) {
    Fail "빌드 산출물을 찾지 못했습니다: $builtExe"
}

# ── 4. .sys로 배치 ───────────────────────────────────────────────────────────

Write-Host "[4/6] .sys로 복사: $OutSys" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path (Split-Path $OutSys) | Out-Null
Copy-Item $builtExe $OutSys -Force

# ── 5. 서명 ──────────────────────────────────────────────────────────────────

if ($SkipSign) {
    Write-Host "[5/6] -SkipSign 지정됨, 서명 생략." -ForegroundColor Yellow
    Write-Host "빌드 완료: $OutSys, $OutHelperExe" -ForegroundColor Green
    exit 0
}

$certDesc = if ($CertThumbprint) { "thumbprint=$CertThumbprint" } else { "/a 자동 선택" }
Write-Host "[5/6] signtool로 드라이버+헬퍼 서명 중 ($certDesc)..." -ForegroundColor Cyan

$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter "signtool.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "\\x64\\" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName

if (-not $signtool) {
    Fail "signtool.exe를 찾지 못했습니다."
}

$certSelectArgs = if ($CertThumbprint) { @("/sha1", $CertThumbprint) } else { @("/a") }

# signtool writes routine informational output to stderr (e.g. the list of
# candidate certificates when using /a), which $ErrorActionPreference = "Stop"
# would otherwise turn into a terminating error before signtool finishes —
# same class of issue as the EWDK env-loading step above.
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
foreach ($target in @($OutSys, $OutHelperExe)) {
    $signArgs = @("sign") + $certSelectArgs + @("/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256", "/v", $target)
    & $signtool @signArgs
    if ($LASTEXITCODE -ne 0) {
        $ErrorActionPreference = $prevEap
        Fail "signtool sign 실패 ($target, exit $LASTEXITCODE)"
    }

    & $signtool verify /pa /v $target
    if ($LASTEXITCODE -ne 0) {
        $ErrorActionPreference = $prevEap
        Fail "signtool verify 실패 ($target, exit $LASTEXITCODE)"
    }
}
$ErrorActionPreference = $prevEap

Write-Host "빌드 + 서명 완료: $OutSys, $OutHelperExe" -ForegroundColor Green
