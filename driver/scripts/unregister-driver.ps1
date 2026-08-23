<#
.SYNOPSIS
    제거/업그레이드 시 WumaDisplayService 커널 드라이버 서비스를 정지하고
    삭제한다. wix7/main.wxs의 UnregisterDriverServiceDeferred 커스텀 액션과
    src-tauri/windows/hooks.nsh의 NSIS_HOOK_PREUNINSTALL이 이 파일을 그대로
    실행한다 (MSI 쪽은 Return="ignore"라 여기서 실패해도 설치/제거 자체는
    계속된다 — 자세한 이유는 main.wxs 주석 참고).
#>
$ErrorActionPreference = "Continue"
$ServiceName = "WumaDisplayService"

sc.exe stop $ServiceName | Out-Null
Start-Sleep -Milliseconds 500
sc.exe delete $ServiceName | Out-Null
exit 0
