# 1. 환경 설정 및 버전 추출
$rawVersion = (Get-Content package.json | ConvertFrom-Json).version
$msixVersion = "$rawVersion.0"
$staging = "msix_staging"
$outputFile = "WumaTracker_test.msix"

Write-Host "--- Step 1: Tauri 빌드 (No Bundle) ---" -ForegroundColor Cyan
# store 피처와 함께 빌드
pnpm tauri build --no-bundle --features store

Write-Host "--- Step 2: 레이아웃 준비 및 에셋 복사 ---" -ForegroundColor Cyan
if (Test-Path $staging) { Remove-Item -Recurse -Force $staging }
New-Item -ItemType Directory -Path "$staging\Assets"

# 매니페스트 버전 치환 및 저장
$templatePath = "src-tauri/windows/AppxManifest.xml"
$manifestContent = [System.IO.File]::ReadAllText((Resolve-Path $templatePath))
$updatedContent = $manifestContent.Replace("PACKAGE_VERSION", $msixVersion)
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText("$staging\AppxManifest.xml", $updatedContent, $utf8NoBom)

# 실행 파일 복사
Copy-Item "src-tauri\target\release\wuma-tracker.exe" -Destination "$staging\WumaTracker.exe"

# --- 아이콘 에셋 복사 (Unplated 버전 포함) ---
$iconSrc = "src-tauri\icons"
Copy-Item "$iconSrc\Square150x150Logo.png" -Destination "$staging\Assets\Square150x150Logo.png"
Copy-Item "$iconSrc\Square44x44Logo.png" -Destination "$staging\Assets\Square44x44Logo.png"
Copy-Item "$iconSrc\StoreLogo.png" -Destination "$staging\Assets\StoreLogo.png"

# 작업 표시줄용 투명 배경 아이콘들
Copy-Item "$iconSrc\Square44x44Logo.png" -Destination "$staging\Assets\Square44x44Logo.targetsize-24_altform-unplated.png"
Copy-Item "$iconSrc\Square44x44Logo.png" -Destination "$staging\Assets\Square44x44Logo.targetsize-32_altform-unplated.png"
Copy-Item "$iconSrc\Square44x44Logo.png" -Destination "$staging\Assets\Square44x44Logo.targetsize-44_altform-unplated.png"
Copy-Item "$iconSrc\Square44x44Logo.png" -Destination "$staging\Assets\Square44x44Logo.targetsize-256_altform-unplated.png"

Write-Host "--- Step 3: Windows SDK 도구(MakeAppx, MakePri) 찾기 ---" -ForegroundColor Cyan
$sdkPath = "C:\Program Files (x86)\Windows Kits\10\bin"
$binFolders = Get-ChildItem -Path $sdkPath -Directory | Where-Object { $_.Name -match "^10\." } | Sort-Object Name -Descending

$makeAppx = $null
$makePri = $null

foreach ($folder in $binFolders) {
    $archPath = Join-Path $folder.FullName "x64"
    if (Test-Path (Join-Path $archPath "makeappx.exe")) {
        $makeAppx = Join-Path $archPath "makeappx.exe"
        $makePri = Join-Path $archPath "makepri.exe"
        break
    }
}

if (-not $makeAppx -or -not $makePri) { Write-Error "SDK 도구를 찾을 수 없습니다."; exit }
Write-Host "Using Tools from: $archPath"

Write-Host "--- Step 4: Resources.pri 인덱스 생성 (아이콘 투명화 핵심) ---" -ForegroundColor Cyan
# 1. 기존 인덱스가 있다면 삭제
if (Test-Path "$staging\resources.pri") { Remove-Item "$staging\resources.pri" }

# 2. PRI 설정 파일 생성 (기본 언어 ko-KR)
& $makePri createconfig /cf "$staging\priconfig.xml" /dq "ko-KR" /pv "10.0.0" /o

# 3. 자산 인덱싱 (이 단계를 거쳐야 unplated 아이콘이 앱에 등록됨)
& $makePri new /pr "$staging" /cf "$staging\priconfig.xml" /of "$staging\resources.pri" /o

# 4. 임시 설정 파일 삭제
if (Test-Path "$staging\priconfig.xml") { Remove-Item "$staging\priconfig.xml" }

Write-Host "--- Step 5: MSIX 패키징 ---" -ForegroundColor Cyan
& $makeAppx pack /d $staging /p $outputFile /nv

Write-Host "--- 결과: $outputFile 생성 완료 ---" -ForegroundColor Green