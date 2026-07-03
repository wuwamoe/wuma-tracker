<#
.SYNOPSIS
    Installs and starts the WumaDisplayService kernel driver for local testing.

.DESCRIPTION
    This driver is Authenticode-signed but not WHQL/attestation-signed, so
    Windows will refuse to load it unless the machine is in test-signing mode
    (or Secure Boot / driver signature enforcement is otherwise relaxed).
    Enable test mode once per test VM:

        bcdedit /set testsigning on
        <reboot>

    "Test Mode" will then appear on the desktop watermark. Never do this on a
    machine used for anything other than throwaway driver testing.

.PARAMETER SysPath
    Path to the built/signed .sys file. Defaults to the build-and-sign.ps1
    output location.

.EXAMPLE
    .\install-driver.ps1
    .\install-driver.ps1 -SysPath C:\path\to\WumaDisplayService.sys
#>
[CmdletBinding()]
param(
    [string]$SysPath = (Join-Path (Split-Path -Parent $PSScriptRoot) "target\WumaDisplayService.sys")
)

$ErrorActionPreference = "Stop"
$ServiceName = "WumaDisplayService"

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)) {
    Write-Error "관리자 권한 PowerShell에서 실행하세요."
    exit 1
}

if (-not (Test-Path $SysPath)) {
    Write-Error "드라이버 파일을 찾지 못했습니다: $SysPath (먼저 build-and-sign.ps1 실행)"
    exit 1
}
$SysPath = (Resolve-Path $SysPath).Path

$existing = sc.exe query $ServiceName 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host "기존 서비스 발견, 중지+삭제 후 재설치합니다." -ForegroundColor Yellow
    sc.exe stop $ServiceName | Out-Null
    Start-Sleep -Milliseconds 500
    sc.exe delete $ServiceName | Out-Null
    Start-Sleep -Milliseconds 500
}

Write-Host "서비스 등록: $ServiceName (파일: $SysPath)" -ForegroundColor Cyan
sc.exe create $ServiceName type= kernel start= demand error= normal binPath= "$SysPath"
if ($LASTEXITCODE -ne 0) {
    Write-Error "sc create 실패 (exit $LASTEXITCODE). 테스트 서명 모드가 켜져 있는지 확인하세요 (bcdedit /set testsigning on 후 재부팅)."
    exit 1
}

Write-Host "드라이버 시작 중..." -ForegroundColor Cyan
sc.exe start $ServiceName
if ($LASTEXITCODE -ne 0) {
    Write-Error "sc start 실패 (exit $LASTEXITCODE). 자세한 원인은 이벤트 뷰어(System 로그, Service Control Manager)를 확인하세요."
    exit 1
}

Write-Host "로드 완료. 확인: sc query $ServiceName" -ForegroundColor Green
