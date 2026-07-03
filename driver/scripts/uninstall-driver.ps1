<#
.SYNOPSIS
    Stops and removes the WumaDisplayService kernel driver.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = "Continue"
$ServiceName = "WumaDisplayService"

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltinRole]::Administrator)) {
    Write-Error "관리자 권한 PowerShell에서 실행하세요."
    exit 1
}

sc.exe stop $ServiceName
Start-Sleep -Milliseconds 500
sc.exe delete $ServiceName

Write-Host "제거 완료." -ForegroundColor Green
