<#
.SYNOPSIS
    Sends a single get-location request to the helper's named pipe and prints
    the parsed response, without needing the full Tauri app running.

.DESCRIPTION
    Connects to \\.\pipe\WumaTrackerHelper, writes the 4-byte little-endian
    opcode 1 (REQ_GET_LOCATION), and reads back the 28-byte GetLocationResponse
    (x,y,z,pitch,yaw,roll as f32 + stage:u8 + 3 bytes padding).

    stage 0   = success, coordinates are valid
    stage 0xFF = driver not open (helper could not reach \\.\WumaDisplayService)
    stage 0xFE = driver open but target process not found
    stage 1..N = driver-reported pointer-chain failure at step N

.PARAMETER TimeoutMs
    How long to wait for the pipe to become available before giving up.
#>
[CmdletBinding()]
param(
    [int]$TimeoutMs = 2000
)

$ErrorActionPreference = "Stop"
$PipeName = "WumaTrackerHelper"

try {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(".", $PipeName, [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect($TimeoutMs)
} catch {
    Write-Error "파이프 연결 실패: $($_.Exception.Message) (헬퍼가 실행 중인지, 이 계정으로 접근 가능한지 확인하세요)"
    exit 1
}

try {
    $req = [BitConverter]::GetBytes([uint32]1) # REQ_GET_LOCATION
    $pipe.Write($req, 0, $req.Length)
    $pipe.Flush()

    $buf = New-Object byte[] 28
    $read = 0
    while ($read -lt $buf.Length) {
        $n = $pipe.Read($buf, $read, $buf.Length - $read)
        if ($n -eq 0) { throw "파이프가 응답 전에 닫혔습니다 (읽은 바이트: $read/28)" }
        $read += $n
    }

    $x = [BitConverter]::ToSingle($buf, 0)
    $y = [BitConverter]::ToSingle($buf, 4)
    $z = [BitConverter]::ToSingle($buf, 8)
    $pitch = [BitConverter]::ToSingle($buf, 12)
    $yaw = [BitConverter]::ToSingle($buf, 16)
    $roll = [BitConverter]::ToSingle($buf, 20)
    $stage = $buf[24]

    $stageDesc = switch ($stage) {
        0    { "성공" }
        255  { "드라이버 미연결 (helper가 \\.\WumaDisplayService를 열지 못함)" }
        254  { "대상 프로세스(Client-Win64-Shipping.exe)를 찾지 못함" }
        default { "드라이버 포인터 체인 실패 (stage=$stage)" }
    }

    Write-Host "stage=$stage ($stageDesc)" -ForegroundColor Cyan
    if ($stage -eq 254) {
        # Bit-exact decode (not the lossy float value) of FindProcessDebug.
        # Uses GetBytes/ToUInt32 rather than SingleToUInt32Bits for
        # compatibility with both Windows PowerShell 5.1 and PowerShell 7+.
        $dbgStatus = [BitConverter]::ToInt32([BitConverter]::GetBytes($x), 0)
        $dbgActualLen = [BitConverter]::ToUInt32([BitConverter]::GetBytes($y), 0)
        $dbgFirstNextEntryOffset = [BitConverter]::ToUInt32([BitConverter]::GetBytes($z), 0)
        $dbgSeen = [BitConverter]::ToUInt32([BitConverter]::GetBytes($pitch), 0)
        $dbgFirstDwordAfterCall = [BitConverter]::ToUInt32([BitConverter]::GetBytes($yaw), 0)
        Write-Host ("debug: status=0x{0:X8} actual_len={1} first_next_entry_offset={2} seen={3} first_dword_after_call=0x{4:X8}" -f $dbgStatus, $dbgActualLen, $dbgFirstNextEntryOffset, $dbgSeen, $dbgFirstDwordAfterCall) -ForegroundColor Yellow
    }
    if ($stage -eq 0) {
        Write-Host ("x={0} y={1} z={2} pitch={3} yaw={4} roll={5}" -f $x, $y, $z, $pitch, $yaw, $roll) -ForegroundColor Green
    }
} finally {
    $pipe.Dispose()
}
