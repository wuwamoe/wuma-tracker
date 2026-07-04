# build-msi.ps1 / sign-msi.ps1이 공유하는 signtool Authenticode 서명 헬퍼.
# 인증서 지문/다이제스트/타임스탬프 URL 기본값은 src-tauri/tauri.conf.json의
# bundle.windows 설정을 그대로 사용한다.

function Invoke-AuthenticodeSign {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [string]$Thumbprint,
        [string]$DigestAlgorithm,
        [string]$TimestampUrl,
        [string]$SignToolPath,
        [string]$Description = "WumaTracker"
    )

    $RepoRoot = $PSScriptRoot
    $SrcTauri = Join-Path $RepoRoot "src-tauri"

    if (-not $Thumbprint -or -not $DigestAlgorithm -or -not $TimestampUrl) {
        $conf = Get-Content (Join-Path $SrcTauri "tauri.conf.json") -Raw | ConvertFrom-Json
        $winCfg = $conf.bundle.windows
        if (-not $Thumbprint) { $Thumbprint = $winCfg.certificateThumbprint }
        if (-not $DigestAlgorithm) { $DigestAlgorithm = $winCfg.digestAlgorithm }
        if (-not $TimestampUrl) { $TimestampUrl = $winCfg.timestampUrl }
    }

    if (-not $Thumbprint) { throw "인증서 지문을 찾을 수 없습니다 (tauri.conf.json bundle.windows.certificateThumbprint 확인)." }
    if (-not $DigestAlgorithm) { $DigestAlgorithm = "sha256" }

    if (-not $SignToolPath) {
        $sdkPath = "C:\Program Files (x86)\Windows Kits\10\bin"
        $SignToolPath = Get-ChildItem -Path $sdkPath -Filter "signtool.exe" -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "x64" -and $_.FullName -match "10\." } |
            Sort-Object FullName -Descending |
            Select-Object -First 1 -ExpandProperty FullName
    }

    if (-not $SignToolPath -or -not (Test-Path $SignToolPath)) {
        throw "signtool.exe를 찾을 수 없습니다. Windows SDK를 설치하거나 -SignToolPath를 직접 지정하세요."
    }

    Write-Host "  파일: $FilePath"
    Write-Host "  signtool: $SignToolPath"
    Write-Host "  지문: $Thumbprint"

    & $SignToolPath sign /sha1 $Thumbprint /fd $DigestAlgorithm /td $DigestAlgorithm /tr $TimestampUrl /d $Description $FilePath
    if ($LASTEXITCODE -ne 0) {
        throw "signtool failed with exit code $LASTEXITCODE"
    }
}
