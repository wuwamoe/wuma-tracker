# WiX v7로 MSI를 직접 패키징한다 (tauri-bundler 내장 WiX v3 대신).
# 1. Tauri 빌드 (패키징 없이, 바이너리 + 프런트엔드만)
# 2. 메인 exe를 signtool로 서명 (안 하면 UAC 승격 요청 시 "확인된 게시자"가 안 뜬다)
# 3. WiX v7로 MSI 패키징 (서명된 exe를 그대로 포함)
#
# MSI 자체는 여기서는 미서명 상태로 나온다. 업데이터 시그니처(.msi.sig)는 signtool로
# msi를 서명한 이후에 sign-msi.ps1이 만드는데, 서명하면 파일 바이트가 바뀌므로
# 여기서 먼저 만들어봤자 어차피 버려진다.
#
# 사전 준비 (최초 1회):
#   dotnet tool install --global wix --version 7.0.0
#   wix eula accept wix7
#   wix extension add WixToolset.UI.wixext -g
#
# 사용법:
#   .\build-msi.ps1 [-Arch x64|x86|arm64] [-Version 1.9.0] [-SkipAppBuild]

param(
    [ValidateSet("x64", "x86", "arm64")]
    [string]$Arch = "x64",
    [string]$Version,
    [switch]$SkipAppBuild
)

$ErrorActionPreference = "Stop"

$RepoRoot = $PSScriptRoot
$SrcTauri = Join-Path $RepoRoot "src-tauri"
$WixDir = Join-Path $SrcTauri "windows\wix7"
$ReleaseDir = Join-Path $SrcTauri "target\release"
$OutDir = Join-Path $RepoRoot "dist"

if (-not $Version) {
    $Version = (Get-Content (Join-Path $SrcTauri "tauri.conf.json") -Raw | ConvertFrom-Json).version
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

if (-not $SkipAppBuild) {
    Write-Host "--- 1단계: Tauri 빌드 (No Bundle) ---" -ForegroundColor Cyan
    bun run tauri build --no-bundle
    if ($LASTEXITCODE -ne 0) { throw "tauri build --no-bundle failed with exit code $LASTEXITCODE" }
}

$MainBinaryPath = Join-Path $ReleaseDir "wuma-tracker.exe"
$IconPath = Join-Path $ReleaseDir "resources\icon.ico"

if (-not (Test-Path $MainBinaryPath)) {
    throw "메인 바이너리를 찾을 수 없습니다: $MainBinaryPath (-SkipAppBuild 없이 먼저 빌드하세요)"
}

Write-Host "--- 2단계: 메인 exe 서명 (signtool) ---" -ForegroundColor Cyan
. "$RepoRoot\Sign-Authenticode.ps1"
Invoke-AuthenticodeSign -FilePath $MainBinaryPath

$MsiPath = Join-Path $OutDir "WumaTracker_${Version}_${Arch}.msi"

Write-Host "--- 3단계: WiX v7 MSI 패키징 ($Arch, v$Version) ---" -ForegroundColor Cyan
wix build "$WixDir\main.wxs" `
    -ext WixToolset.UI.wixext `
    -loc "$WixDir\locale.wxl" `
    -bindpath $WixDir `
    -arch $Arch `
    -d "ProductVersion=$Version" `
    -d "MainBinaryPath=$MainBinaryPath" `
    -d "IconPath=$IconPath" `
    -o $MsiPath

if ($LASTEXITCODE -ne 0) {
    throw "wix build failed with exit code $LASTEXITCODE"
}

Write-Host "`n--- 완료 (msi 자체는 미서명, sign-msi.ps1 실행 필요) ---" -ForegroundColor Green
Write-Host "MSI: $MsiPath"
