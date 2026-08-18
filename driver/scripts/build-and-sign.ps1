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
$OutSys = Join-Path $DriverDir "target\WumaDisplayService.sys"

function Fail($msg) {
    Write-Error $msg
    exit 1
}

$setupBuildEnv = Join-Path $EwdkRoot "BuildEnv\SetupBuildEnv.cmd"
if (-not (Test-Path $setupBuildEnv)) {
    Fail "EWDK를 찾을 수 없습니다: $setupBuildEnv (-EwdkRoot 확인)"
}

# ── 1. EWDK 빌드 환경 로드 (cmd에서 실행 후 env를 PowerShell로 가져옴) ──────────

Write-Host "[1/5] EWDK 빌드 환경 로드 중 ($EwdkRoot, $Arch)..." -ForegroundColor Cyan

$envDumpFile = New-TemporaryFile
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$cmdLine = "call `"$setupBuildEnv`" $Arch >nul && set"
cmd.exe /d /s /c $cmdLine > $envDumpFile.FullName 2>&1
$ErrorActionPreference = $prevEap

$envVars = [System.Collections.Generic.Dictionary[string, string]]::new([System.StringComparer]::Ordinal)
Get-Content $envDumpFile.FullName | ForEach-Object {
    if ($_ -match '^([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
        $envVars[$matches[1]] = $matches[2]
    }
}

if (-not $envVars.ContainsKey("WDKContentRoot") -or -not $envVars.ContainsKey("Version_Number")) {
    Write-Warning "SetupBuildEnv.cmd 환경 덤프를 읽지 못했습니다. EWDK 트리에서 빌드 환경을 직접 구성합니다."
    $sdkRootFallback = Join-Path $EwdkRoot "Program Files\Windows Kits\10"
    $sdkVersionDirFallback = Get-ChildItem (Join-Path $sdkRootFallback "Include") -Directory |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
        Sort-Object Name -Descending |
        Select-Object -First 1
    if (-not $sdkVersionDirFallback) {
        $envDumpPreview = (Get-Content $envDumpFile.FullName -ErrorAction SilentlyContinue | Select-Object -First 40) -join "`n"
        Remove-Item $envDumpFile.FullName -ErrorAction SilentlyContinue
        Fail "EWDK SDK 버전을 찾지 못했습니다. SetupBuildEnv.cmd 출력:`n$envDumpPreview"
    }

    $msvcToolsRootFallback = Join-Path $EwdkRoot "Program Files\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC"
    $msvcVersionDirFallback = Get-ChildItem $msvcToolsRootFallback -Directory |
        Sort-Object Name -Descending |
        Select-Object -First 1
    if (-not $msvcVersionDirFallback) {
        Fail "MSVC 툴셋을 찾지 못했습니다: $msvcToolsRootFallback"
    }

    $envVars["WDKContentRoot"] = "$sdkRootFallback\"
    $envVars["Version_Number"] = $sdkVersionDirFallback.Name
    $envVars["PATH"] = @(
        (Join-Path $msvcVersionDirFallback.FullName "bin\Hostx64\x64"),
        (Join-Path $sdkRootFallback "bin\x64"),
        (Join-Path $sdkRootFallback "Tools\$($sdkVersionDirFallback.Name)\x64"),
        $env:Path
    ) -join ";"
    $envVars["INCLUDE"] = @(
        (Join-Path $msvcVersionDirFallback.FullName "include"),
        (Join-Path $sdkRootFallback "Include\$($sdkVersionDirFallback.Name)\km"),
        (Join-Path $sdkRootFallback "Include\$($sdkVersionDirFallback.Name)\shared"),
        (Join-Path $sdkRootFallback "Include\$($sdkVersionDirFallback.Name)\ucrt"),
        (Join-Path $sdkRootFallback "Include\$($sdkVersionDirFallback.Name)\um")
    ) -join ";"
    $envVars["LIB"] = @(
        (Join-Path $sdkRootFallback "Lib\$($sdkVersionDirFallback.Name)\km\x64"),
        (Join-Path $sdkRootFallback "Lib\$($sdkVersionDirFallback.Name)\um\x64"),
        (Join-Path $sdkRootFallback "Lib\$($sdkVersionDirFallback.Name)\ucrt\x64"),
        (Join-Path $msvcVersionDirFallback.FullName "lib\x64")
    ) -join ";"
}
Remove-Item $envDumpFile.FullName -ErrorAction SilentlyContinue

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

$certDesc = if ($CertThumbprint) { "thumbprint=$CertThumbprint" } else { "/a 자동 선택" }
Write-Host "[5/5] signtool로 드라이버 서명 중 ($certDesc)..." -ForegroundColor Cyan

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
$signArgs = @("sign") + $certSelectArgs + @("/fd", "SHA256", "/tr", $TimestampUrl, "/td", "SHA256", "/v", $OutSys)
& $signtool @signArgs
if ($LASTEXITCODE -ne 0) {
    $ErrorActionPreference = $prevEap
    Fail "signtool sign 실패 ($OutSys, exit $LASTEXITCODE)"
}

& $signtool verify /pa /v $OutSys
if ($LASTEXITCODE -ne 0) {
    $ErrorActionPreference = $prevEap
    Fail "signtool verify 실패 ($OutSys, exit $LASTEXITCODE)"
}
$ErrorActionPreference = $prevEap

Write-Host "빌드 + 서명 완료: $OutSys" -ForegroundColor Green
