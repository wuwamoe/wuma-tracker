# 1. 파트너 센터 정보 (매니페스트와 일치해야 함)
$publisher = "CN=4CC8BC66-0841-4C8D-A1D1-C6694C39EB15"
$msixFile = "WumaTracker_test.msix"

if (-not (Test-Path $msixFile)) {
    Write-Error "파일을 찾을 수 없습니다: $msixFile"
    exit
}

Write-Host "--- 1단계: 로컬 컴퓨터 저장소에서 인증서 확인/생성 ---" -ForegroundColor Cyan
$cert = Get-ChildItem Cert:\LocalMachine\My | Where-Object { $_.Subject -eq $publisher } | Select-Object -First 1

if (-not $cert) {
    $cert = New-SelfSignedCertificate -Type Custom -Subject $publisher `
            -KeyUsage DigitalSignature -FriendlyName "WumaTracker Local Test" `
            -CertStoreLocation "Cert:\LocalMachine\My" `
            -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    Write-Host "새 인증서를 생성했습니다." -ForegroundColor Green
} else {
    Write-Host "기존 인증서를 사용합니다." -ForegroundColor Yellow
}

Write-Host "--- 2단계: 신뢰할 수 있는 루트 기관에 등록 확인 ---" -ForegroundColor Cyan
$rootCert = Get-ChildItem Cert:\LocalMachine\Root | Where-Object { $_.Thumbprint -eq $cert.Thumbprint }
if (-not $rootCert) {
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store("Root", "LocalMachine")
    $store.Open("ReadWrite")
    $store.Add($cert)
    $store.Close()
    Write-Host "루트 기관에 등록 완료." -ForegroundColor Green
}

Write-Host "--- 3단계: Signtool 서명 (Local Machine 저장소 명시) ---" -ForegroundColor Cyan
$sdkPath = "C:\Program Files (x86)\Windows Kits\10\bin"
$signtool = Get-ChildItem -Path $sdkPath -Filter "signtool.exe" -Recurse |
            Where-Object { $_.FullName -match "x64" -and $_.FullName -match "10\." } |
            Sort-Object -Property FullName -Descending |
            Select-Object -First 1 -ExpandProperty FullName

# 지문에서 공백 제거 및 문자열 변환
$thumb = $cert.Thumbprint.Replace(" ", "").Trim()

# /sm: 로컬 컴퓨터 저장소 사용 (중요)
# /s My: '개인' 저장소 지정
& $signtool sign /fd SHA256 /sha1 $thumb /sm /s My /v $msixFile

Write-Host "`n--- 모든 과정 완료! ---" -ForegroundColor Green