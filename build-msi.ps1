# WiX v7로 MSI를 직접 패키징한다 (tauri-bundler 내장 WiX v3 대신).
# 1. Tauri 빌드 (패키징 없이, 바이너리 + 프런트엔드만)
# 2. 메인 exe를 signtool로 서명 (안 하면 UAC 승격 요청 시 "확인된 게시자"가 안 뜬다)
# 3. 커널 드라이버(.sys)는 여기서 빌드/서명하지 않는다 — EWDK 환경이 필요하고
#    Authenticode 서명 인증서가 signtool 자동 서명 후보로 노출되어 있어야 하는 등
#    준비 단계가 무겁고, 최종적으로는 Attestation Signing까지 받아야 하는 별도
#    파이프라인(driver/scripts/build-and-sign.ps1, prepare-attestation-package.ps1)의
#    산출물이다. 여기서는 이미 Attestation 서명된 driver/signed/WumaDisplayService.sys를
#    그대로 복사해 패키징에 포함하기만 한다. driver/target/은 로컬 빌드/테스트용
#    산출물이라 .gitignore돼 있어서 커밋되지 않는다 — driver/signed/는 그와 별개로,
#    "지금 출시에 실제로 실려나가는 서명된 드라이버"를 git에 커밋해두는 자리다.
#    자체 CI 러너를 포함해 저장소를 새로 클론한 어떤 환경에서도 별도로 파일을
#    가져다놓지 않고 바로 MSI를 만들 수 있게 하기 위함. 드라이버를 다시
#    Attestation 서명받을 때마다 이 파일도 같이 새로 커밋해야 한다 — 안 그러면
#    소스(driver/src/main.rs)와 여기 커밋된 바이너리가 조용히 어긋난다.
# 4. WiX v7로 MSI 패키징 (서명된 exe + 이미 서명된 드라이버 포함)
#
# 헬퍼 서비스는 더 이상 없다 — 앱이 드라이버를 직접 연다
# (driver/docs/DIRECT-ACCESS-PLAN.md 참고).
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
#   .\build-msi.ps1 [-Arch x64|x86|arm64] [-Version 1.9.0] [-SkipAppBuild] [-DriverSysPath ...]

param(
    [ValidateSet("x64", "x86", "arm64")]
    [string]$Arch = "x64",
    [string]$Version,
    [switch]$SkipAppBuild,
    [string]$DriverSysPath
)

$ErrorActionPreference = "Stop"

$RepoRoot = $PSScriptRoot
$SrcTauri = Join-Path $RepoRoot "src-tauri"
$WixDir = Join-Path $SrcTauri "windows\wix7"
$ReleaseDir = Join-Path $SrcTauri "target\release"
$DriverDir = Join-Path $RepoRoot "driver"
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
$IconPath = Join-Path $ReleaseDir "resources\icon.ico"

if (-not (Test-Path $MainBinaryPath)) {
    throw "메인 바이너리를 찾을 수 없습니다: $MainBinaryPath (-SkipAppBuild 없이 먼저 빌드하세요)"
}

Write-Host "--- 2단계: 메인 exe 서명 (signtool) ---" -ForegroundColor Cyan
. "$RepoRoot\Sign-Authenticode.ps1"
Invoke-AuthenticodeSign -FilePath $MainBinaryPath

if (-not (Test-Path $DriverSysPath)) {
    throw "서명된 드라이버를 찾지 못했습니다: $DriverSysPath (Attestation 서명 완료 후 driver/signed/WumaDisplayService.sys로 복사해뒀는지 확인하세요)"
}

$RegisterDriverScriptPath = Join-Path $DriverDir "scripts\msi-register-driver.ps1"
$UnregisterDriverScriptPath = Join-Path $DriverDir "scripts\msi-unregister-driver.ps1"

$MsiPath = Join-Path $OutDir "WumaTracker_${Version}_${Arch}.msi"

Write-Host "--- 4단계: WiX v7 MSI 패키징 ($Arch, v$Version) ---" -ForegroundColor Cyan
wix build "$WixDir\main.wxs" `
    -ext WixToolset.UI.wixext `
    -loc "$WixDir\locale.wxl" `
    -bindpath $WixDir `
    -arch $Arch `
    -d "ProductVersion=$Version" `
    -d "MainBinaryPath=$MainBinaryPath" `
    -d "IconPath=$IconPath" `
    -d "DriverSysPath=$DriverSysPath" `
    -d "RegisterDriverScriptPath=$RegisterDriverScriptPath" `
    -d "UnregisterDriverScriptPath=$UnregisterDriverScriptPath" `
    -o $MsiPath

if ($LASTEXITCODE -ne 0) {
    throw "wix build failed with exit code $LASTEXITCODE"
}

Write-Host "`n--- 완료 (msi 자체는 미서명, sign-msi.ps1 실행 필요) ---" -ForegroundColor Green
Write-Host "MSI: $MsiPath"
