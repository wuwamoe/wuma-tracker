<#
.SYNOPSIS
    Runs `cargo check` for the WumaTracker kernel driver using an Enterprise WDK (EWDK)
    build environment, without building or signing. Mirrors build-and-sign.ps1's EWDK
    environment setup.

.PARAMETER EwdkRoot
    Path to an extracted/mounted EWDK image (contains LaunchBuildEnv.cmd).

.EXAMPLE
    .\ewdk-check.ps1
    .\ewdk-check.ps1 -EwdkRoot A:\EWDK
#>
[CmdletBinding()]
param(
    [string]$EwdkRoot = "A:\EWDK",
    [string]$Arch = "amd64"
)

$ErrorActionPreference = "Stop"

$DriverDir = Split-Path -Parent $PSScriptRoot

function Fail($msg) {
    Write-Error $msg
    exit 1
}

$setupBuildEnv = Join-Path $EwdkRoot "BuildEnv\SetupBuildEnv.cmd"
if (-not (Test-Path $setupBuildEnv)) {
    Fail "EWDK를 찾을 수 없습니다: $setupBuildEnv (-EwdkRoot 확인)"
}

Write-Host "[1/3] EWDK 빌드 환경 로드 중 ($EwdkRoot, $Arch)..." -ForegroundColor Cyan

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

Write-Host "[2/3] 링커 LIB 경로 보강 중..." -ForegroundColor Cyan

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

Write-Host "[3/3] cargo check ($DriverDir)..." -ForegroundColor Cyan

Push-Location $DriverDir
try {
    & cargo check
    if ($LASTEXITCODE -ne 0) {
        Fail "cargo check 실패 (exit $LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

Write-Host "cargo check 통과." -ForegroundColor Green
