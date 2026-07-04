# build-msi.ps1로 만든 MSI를 signtool로 Authenticode 서명하고, 서명으로 바뀐 바이트에
# 맞춰 Tauri 업데이터 시그니처(.msi.sig)를 다시 생성한다.
#
# 서명 전에 만든 .msi.sig는 파일 바이트가 바뀌면 무효가 되므로, 서명 후 반드시
# 시그니처를 재생성한다.
#
# 환경변수:
#   TAURI_SIGNING_PRIVATE_KEY          (필수) 업데이터 서명용 개인키
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD (선택) 개인키 비밀번호
#
# 사용법:
#   .\sign-msi.ps1 -MsiPath .\dist\WumaTracker_1.9.0_x64.msi

param(
    [Parameter(Mandatory = $true)]
    [string]$MsiPath,
    [string]$Thumbprint,
    [string]$DigestAlgorithm,
    [string]$TimestampUrl,
    [string]$SignToolPath
)

$ErrorActionPreference = "Stop"

$MsiPath = Resolve-Path $MsiPath
$RepoRoot = $PSScriptRoot

Write-Host "--- 1단계: signtool 서명 ---" -ForegroundColor Cyan
. "$RepoRoot\Sign-Authenticode.ps1"
Invoke-AuthenticodeSign -FilePath $MsiPath -Thumbprint $Thumbprint -DigestAlgorithm $DigestAlgorithm -TimestampUrl $TimestampUrl -SignToolPath $SignToolPath

Write-Host "--- 2단계: 업데이터 시그니처(.msi.sig) 재생성 ---" -ForegroundColor Cyan
if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
    throw "TAURI_SIGNING_PRIVATE_KEY가 설정되어 있지 않습니다."
}

$sigPath = "$MsiPath.sig"
if (Test-Path $sigPath) { Remove-Item $sigPath -Force }

Push-Location $RepoRoot
try {
    bun run tauri signer sign $MsiPath
    if ($LASTEXITCODE -ne 0) { throw "tauri signer sign failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

Write-Host "`n--- 완료 ---" -ForegroundColor Green
Write-Host "서명된 MSI: $MsiPath"
Write-Host "시그니처: $sigPath"
