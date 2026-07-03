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
    SHA1 thumbprint of the code-signing certificate in the current user's certificate
    store (CurrentUser\My). Required unless -SkipSign is passed.

.PARAMETER TimestampUrl
    RFC 3161 timestamp server URL. Certum's is used by default since that's the CA in use.

.PARAMETER SkipSign
    Build only; skip the signtool step (useful for quick compile checks).

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
$OutSys = Join-Path $DriverDir "target\WumaDisplayService.sys"

function Fail($msg) {
    Write-Error $msg
    exit 1
}

if (-not $SkipSign -and -not $CertThumbprint) {
    Fail "CertThumbprint이 필요합니다 (서명을 건너뛰려면 -SkipSign 사용)."
}

$setupBuildEnv = Join-Path $EwdkRoot "BuildEnv\SetupBuildEnv.cmd"
if (-not (Test-Path $setupBuildEnv)) {
    Fail "EWDK를 찾을 수 없습니다: $setupBuildEnv (-EwdkRoot 확인)"
}

# ── 1. EWDK 빌드 환경 로드 (cmd에서 실행 후 env를 PowerShell로 가져옴) ──────────

Write-Host "[1/5] EWDK 빌드 환경 로드 중 ($EwdkRoot, $Arch)..." -ForegroundColor Cyan

$envDumpFile = New-TemporaryFile
cmd /c "`"$setupBuildEnv`" $Arch && set" > $envDumpFile.FullName 2>&1

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

Write-Host "[2/5] 링커 LIB 경로 보강 중..." -ForegroundColor Cyan

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

Write-Host "[3/5] cargo build --release ($DriverDir)..." -ForegroundColor Cyan

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

Write-Host "[4/5] .sys로 복사: $OutSys" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path (Split-Path $OutSys) | Out-Null
Copy-Item $builtExe $OutSys -Force

# ── 5. 서명 ──────────────────────────────────────────────────────────────────

if ($SkipSign) {
    Write-Host "[5/5] -SkipSign 지정됨, 서명 생략." -ForegroundColor Yellow
    Write-Host "빌드 완료: $OutSys" -ForegroundColor Green
    exit 0
}

Write-Host "[5/5] signtool로 서명 중 (thumbprint=$CertThumbprint)..." -ForegroundColor Cyan

$signtool = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter "signtool.exe" -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match "\\x64\\" } |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName

if (-not $signtool) {
    Fail "signtool.exe를 찾지 못했습니다."
}

& $signtool sign /sha1 $CertThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 /v $OutSys
if ($LASTEXITCODE -ne 0) {
    Fail "signtool sign 실패 (exit $LASTEXITCODE)"
}

& $signtool verify /pa /v $OutSys
if ($LASTEXITCODE -ne 0) {
    Fail "signtool verify 실패 (exit $LASTEXITCODE)"
}

Write-Host "빌드 + 서명 완료: $OutSys" -ForegroundColor Green
