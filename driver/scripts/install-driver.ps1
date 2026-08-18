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

    # 앱(wuma-tracker.exe)이 디바이스 핸들을 열어둔 채로 떠 있으면 드라이버 언로드가
    # 끝나지 않아 서비스가 "삭제 대기(marked for deletion, exit 1072)" 상태로 걸릴 수
    # 있다. win_proc_driver.rs는 poll 1회마다 핸들을 열고 바로 닫으므로 보통 문제가
    # 안 되지만, 앱이 poll 도중에 멈춰 있으면 여전히 걸릴 수 있어 안전하게 종료한다.
    Stop-Process -Name wuma-tracker -Force -ErrorAction SilentlyContinue

    sc.exe stop $ServiceName | Out-Null

    # sc stop은 비동기다: 실제로 STOPPED가 될 때까지 폴링하지 않으면 뒤이은
    # delete가 "아직 멈추는 중" 상태의 서비스에 대해 실행되어 1072로 걸릴 수 있다.
    $stopped = $false
    for ($i = 0; $i -lt 20; $i++) {
        $state = (sc.exe query $ServiceName 2>$null) -join "`n"
        if ($state -match "STOPPED") { $stopped = $true; break }
        Start-Sleep -Milliseconds 500
    }
    if (-not $stopped) {
        Write-Warning "서비스가 10초 안에 STOPPED 상태가 되지 않았습니다. 계속 진행하지만 재부팅이 필요할 수 있습니다."
    }

    sc.exe delete $ServiceName | Out-Null

    # delete도 비동기라 SCM 데이터베이스에서 완전히 제거될 때까지 약간의 지연이
    # 있을 수 있다. 다음 create가 1072로 실패하지 않도록 짧게 재시도한다.
    for ($i = 0; $i -lt 10; $i++) {
        sc.exe query $ServiceName 2>$null | Out-Null
        if ($LASTEXITCODE -ne 0) { break }
        Start-Sleep -Milliseconds 500
    }
}

Write-Host "서비스 등록: $ServiceName (파일: $SysPath)" -ForegroundColor Cyan
$created = $false
for ($i = 0; $i -lt 6; $i++) {
    sc.exe create $ServiceName type= kernel start= demand error= normal binPath= "$SysPath"
    if ($LASTEXITCODE -eq 0) { $created = $true; break }
    if ($LASTEXITCODE -ne 1072) { break }
    Write-Host "서비스가 아직 삭제 대기 중 (1072), 재시도..." -ForegroundColor Yellow
    Start-Sleep -Milliseconds 500
}
if (-not $created) {
    Write-Error "sc create 실패 (exit $LASTEXITCODE). 1072(삭제 대기)가 계속되면 재부팅이 필요할 수 있고, 그 외에는 테스트 서명 모드가 켜져 있는지 확인하세요 (bcdedit /set testsigning on 후 재부팅)."
    exit 1
}

Write-Host "등록된 ImagePath 확인 (sc qc):" -ForegroundColor DarkGray
sc.exe qc $ServiceName

Write-Host "드라이버 시작 중..." -ForegroundColor Cyan
sc.exe start $ServiceName
if ($LASTEXITCODE -ne 0) {
    Write-Error "sc start 실패 (exit $LASTEXITCODE). 위 sc qc 출력의 BINARY_PATH_NAME 값을 확인하세요. 자세한 원인은 이벤트 뷰어(System 로그, Service Control Manager)도 참고하세요."
    exit 1
}

Write-Host "로드 완료. 확인: sc query $ServiceName" -ForegroundColor Green
