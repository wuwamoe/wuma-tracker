<#
.SYNOPSIS
    설치/업그레이드/리페어 시 WumaDisplayService 커널 드라이버 서비스를
    등록하고 시작한다. wix7/main.wxs의 RegisterDriverServiceDeferred 커스텀
    액션과 src-tauri/windows/hooks.nsh의 NSIS_HOOK_POSTINSTALL이 이 파일을
    그대로 실행한다 — MSI/NSIS 두 인스톨러가 공유하는 스크립트다.

.DESCRIPTION
    (MSI 쪽 한정) 이 로직을 XML 속성(ExeCommand) 안에 한 줄짜리 PowerShell로
    박아넣지 않고 평범한 .ps1 파일로 둔 이유: MSI의 "Formatted" 문자열 문법은
    `{`/`}`를 조건부 텍스트 블록, `[...]`를 프로퍼티 참조로 예약해서 쓴다.
    PowerShell의 if/for 블록 자체가 중괄호투성이라, 인라인으로 넣으면 그
    중괄호/브래킷을 전부 `[\{]`/`[\}]`/`[\[]`/`[\]]`로 손으로 이스케이프해야
    하고 (실제로 이걸 빠뜨려서 MSI 오류 1722로 몇 번 막혔었다),
    CustomActionData를 가리키는 실제 토큰(`[~]`)도 그 안에서 기대대로 동작하지
    않았다. 스크립트 파일로 빼면 이 문제 자체가 사라진다 — ExeCommand는 파일
    경로 + 인자 하나뿐인 짧은 한 줄로 남는다.

    install-driver.ps1(로컬 개발용 수동 설치)과 로직이 거의 같다 — 두 곳 다
    "이미 등록돼 있으면 먼저 지우고 기다린 다음 새로 만든다"는 자가치유 패턴을
    쓴다. 재설치/리페어(REINSTALLMODE=amus)는 MajorUpgrade의
    RemoveExistingProducts를 안 타서 기존 서비스가 그대로 남아있는 채로 이
    스크립트가 실행되므로, 이 선행 정리 없이 바로 `sc create`를 하면 1073(이미
    존재함)으로 실패한다.

.PARAMETER SysPath
    설치된 드라이버 .sys 파일의 경로.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$SysPath
)

$ErrorActionPreference = "Stop"
$ServiceName = "WumaDisplayService"

$existing = sc.exe query $ServiceName 2>$null
if ($LASTEXITCODE -eq 0) {
    sc.exe stop $ServiceName | Out-Null

    for ($i = 0; $i -lt 20; $i++) {
        $state = (sc.exe query $ServiceName 2>$null) -join "`n"
        if ($state -match "STOPPED") { break }
        Start-Sleep -Milliseconds 500
    }

    sc.exe delete $ServiceName | Out-Null

    for ($i = 0; $i -lt 10; $i++) {
        sc.exe query $ServiceName 2>$null | Out-Null
        if ($LASTEXITCODE -ne 0) { break }
        Start-Sleep -Milliseconds 500
    }
}

$created = $false
for ($i = 0; $i -lt 10; $i++) {
    sc.exe create $ServiceName type= kernel start= demand error= normal binPath= "$SysPath" | Out-Null
    if ($LASTEXITCODE -eq 0) { $created = $true; break }
    if ($LASTEXITCODE -ne 1072) { exit $LASTEXITCODE }
    Start-Sleep -Milliseconds 500
}
if (-not $created) { exit $LASTEXITCODE }

sc.exe start $ServiceName | Out-Null
exit 0
