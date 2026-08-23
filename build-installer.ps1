# Builds the WPF installer app (installer/WumaTracker.Setup) as a single
# self-contained exe. Replaces both the old `tauri build --bundles nsis`
# path and the retired hand-authored NSIS attempt (build-nsis.ps1, kept in
# git history but no longer used) — see specs/0005-wpf-installer.md in the
# wuma-base workspace for why.
#
# Steps:
#   1. Tauri build (no bundling — just the binary + frontend)
#   2. Sign the main exe with signtool (needed for UAC's "Verified
#      publisher", same reason build-msi.ps1/build-nsis.ps1 do this)
#   3. Driver .sys is not built/signed here — copies the already
#      Attestation-signed driver/signed/WumaDisplayService.sys, same as
#      build-msi.ps1.
#   4. Stage payload/ inside the installer project (embedded as resources
#      at publish time — see WumaTracker.Setup.csproj) and `dotnet publish`
#      a single-file win-x64 exe.
#
# The installer itself comes out unsigned here. The updater signature
# (.exe.sig) is made the same way sign-msi.ps1 makes the MSI's, after
# signtool runs on the installer exe (signing changes the file bytes).
#
# Usage:
#   .\build-installer.ps1 [-Version 2.1.0] [-SkipAppBuild] [-SkipSign] [-DriverSysPath ...]

param(
    [string]$Version,
    [switch]$SkipAppBuild,
    [switch]$SkipSign,
    [string]$DriverSysPath
)

$ErrorActionPreference = "Stop"

$RepoRoot = $PSScriptRoot
$SrcTauri = Join-Path $RepoRoot "src-tauri"
$ReleaseDir = Join-Path $SrcTauri "target\release"
$DriverDir = Join-Path $RepoRoot "driver"
$InstallerProjectDir = Join-Path $RepoRoot "installer\WumaTracker.Setup"
$PayloadDir = Join-Path $InstallerProjectDir "payload"
$CompressorProjectDir = Join-Path $RepoRoot "installer\PayloadCompressor"
$OutDir = Join-Path $RepoRoot "dist"

if (-not $Version) {
    $Version = (Get-Content (Join-Path $SrcTauri "tauri.conf.json") -Raw | ConvertFrom-Json).version
}
if (-not $DriverSysPath) {
    $DriverSysPath = Join-Path $DriverDir "signed\WumaDisplayService.sys"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

if (-not $SkipAppBuild) {
    Write-Host "--- 1단계: Tauri 빌드 (No Bundle) ---" -ForegroundColor Cyan
    bun run tauri build --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "tauri build --no-bundle failed with exit code $LASTEXITCODE" }
}

$MainBinaryPath = Join-Path $ReleaseDir "wuma-tracker.exe"
if (-not (Test-Path $MainBinaryPath)) {
    throw "메인 바이너리를 찾을 수 없습니다: $MainBinaryPath (-SkipAppBuild 없이 먼저 빌드하세요)"
}

if ($SkipSign) {
    Write-Host "--- 2단계: -SkipSign 지정됨, 메인 exe 서명 생략 ---" -ForegroundColor Yellow
} else {
    Write-Host "--- 2단계: 메인 exe 서명 (signtool) ---" -ForegroundColor Cyan
    . "$RepoRoot\Sign-Authenticode.ps1"
    Invoke-AuthenticodeSign -FilePath $MainBinaryPath
}

if (-not (Test-Path $DriverSysPath)) {
    throw "서명된 드라이버를 찾지 못했습니다: $DriverSysPath (Attestation 서명 완료 후 driver/signed/WumaDisplayService.sys로 복사해뒀는지 확인하세요)"
}

Write-Host "--- 3단계: payload 준비 (Brotli 압축) ---" -ForegroundColor Cyan
# Installer.cs's ExtractPayload decompresses with BrotliSharpLib — net472
# has no PublishSingleFile-style bundle compression to lean on, and
# embedded resources are otherwise stored as-is, which is most of why the
# exe was ~30MB bigger than it needed to be (wuma-tracker.exe alone is
# ~29MB raw). Brotli over gzip/deflate: measured on the real payload, it
# matches xz/LZMA2 (what NSIS/7-Zip use) to within ~1%.
Write-Host "  (installer/PayloadCompressor 빌드 중...)"
dotnet build -c Release $CompressorProjectDir | Out-Null
if ($LASTEXITCODE -ne 0) { throw "PayloadCompressor build failed with exit code $LASTEXITCODE" }
$Compressor = Join-Path $CompressorProjectDir "bin\Release\net8.0\PayloadCompressor.exe"

function Compress-PayloadFile($SourcePath, $DestPath) {
    & $Compressor $SourcePath $DestPath
    if ($LASTEXITCODE -ne 0) { throw "PayloadCompressor failed on $SourcePath with exit code $LASTEXITCODE" }
}

if (Test-Path $PayloadDir) { Remove-Item $PayloadDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $PayloadDir | Out-Null
Compress-PayloadFile $MainBinaryPath (Join-Path $PayloadDir "wuma-tracker.exe")
Compress-PayloadFile $DriverSysPath (Join-Path $PayloadDir "WumaDisplayService.sys")
Compress-PayloadFile (Join-Path $DriverDir "scripts\register-driver.ps1") (Join-Path $PayloadDir "register-driver.ps1")
Compress-PayloadFile (Join-Path $DriverDir "scripts\unregister-driver.ps1") (Join-Path $PayloadDir "unregister-driver.ps1")

Write-Host "--- 4단계: dotnet publish (v$Version) ---" -ForegroundColor Cyan
# .NET Framework (net472) — no -r/--self-contained/PublishSingleFile here,
# those are modern-.NET-only concepts. The published exe just needs the
# WPF assemblies already in the Windows GAC.
Push-Location $InstallerProjectDir
try {
    dotnet publish -c Release -p:Version=$Version -o "$OutDir\installer-publish"
    if ($LASTEXITCODE -ne 0) { throw "dotnet publish failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

$OutFile = Join-Path $OutDir "WumaTracker_${Version}_x64-setup.exe"
Copy-Item (Join-Path $OutDir "installer-publish\WumaTracker.Setup.exe") $OutFile -Force

Write-Host "`n--- 완료 (설치 프로그램 자체는 미서명, sign-msi.ps1과 같은 방식으로 서명 필요) ---" -ForegroundColor Green
Write-Host "Setup: $OutFile"
