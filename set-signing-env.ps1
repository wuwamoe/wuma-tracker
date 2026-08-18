# TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)를 현재 셸에 설정한다. dot-source로 실행해야
# 환경변수가 이 스크립트 종료 후에도 셸에 남는다 — 그냥 실행하면(`.\set-signing-env.ps1`)
# 자식 프로세스에서만 설정되고 부모 셸에는 반영되지 않는다.
#
# TAURI_SIGNING_PRIVATE_KEY는 경로가 아니라 키 파일의 "내용"을 그대로 받는다 —
# `tauri signer generate`로 만든 키 파일 자체가 이미 (그 안에 untrusted comment
# 등을 포함한 실제 rsign 키 텍스트를) base64로 인코딩한 한 줄짜리 블롭이라,
# 경로 문자열을 넣으면 그 문자열 자체를 base64로 디코드하려다 실패한다
# (`Invalid symbol 58` 에러 — "C:" 의 콜론에서 걸림). 그래서 파일 내용을 읽어서
# 그대로 넣어야 한다.
#
# 사용법:
#   . .\set-signing-env.ps1
#   . .\set-signing-env.ps1 -KeyPath key\myapp.key -Password "..."

param(
    [string]$KeyPath = "key\myapp.key",
    [string]$Password
)

$RepoRoot = $PSScriptRoot
$ResolvedKeyPath = Join-Path $RepoRoot $KeyPath

if (-not (Test-Path $ResolvedKeyPath)) {
    Write-Error "개인키를 찾지 못했습니다: $ResolvedKeyPath"
    return
}

$env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content $ResolvedKeyPath -Raw).Trim()

# 비밀번호가 없는 키라도 이 환경변수를 아예 안 만들면 `tauri signer sign`이
# 터미널에 비밀번호를 대화형으로 물어본다 — 이 도구를 스크립트/CI처럼 표준입력이
# 없는 환경에서 돌리면 그 프롬프트에서 영원히 멈춘다. 그래서 -Password를 안 줘도
# 항상 (빈 문자열이라도) 설정해서 비대화형으로 만든다.
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = if ($Password) { $Password } else { "" }

Write-Host "TAURI_SIGNING_PRIVATE_KEY 설정됨 (원본: $ResolvedKeyPath)" -ForegroundColor Green
if ($Password) {
    Write-Host "TAURI_SIGNING_PRIVATE_KEY_PASSWORD 설정됨" -ForegroundColor Green
} else {
    Write-Host "TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (빈 문자열 — 비밀번호 없는 키)" -ForegroundColor Yellow
}
