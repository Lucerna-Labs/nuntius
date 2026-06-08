$ErrorActionPreference = "Stop"

$project = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $project "target\debug\ccc-cli.exe"
$url = "http://127.0.0.1:3000/admin"
$outLog = Join-Path $project "nuntius-server.out.log"
$errLog = Join-Path $project "nuntius-server.err.log"

function Get-NuntiusServer {
    # Prefer CIM (gives ExecutablePath + CommandLine). On locked-down
    # Windows containers / corporate boxes WMI may be unavailable, so
    # fall back to Get-Process which only confirms the binary is
    # running by name (no path / cmdline check).
    try {
        $procs = Get-CimInstance Win32_Process -Filter "Name = 'ccc-cli.exe'" -ErrorAction Stop
        $procs | Where-Object {
            $_.ExecutablePath -eq $exe -and
            $_.CommandLine -match "(^|\\s)serve(\\s|$)"
        }
    } catch {
        Write-Verbose "Get-CimInstance failed ($($_.Exception.Message)); falling back to Get-Process"
        Get-Process -Name 'ccc-cli' -ErrorAction SilentlyContinue | Where-Object {
            $_.Path -eq $exe
        }
    }
}

if (-not (Get-NuntiusServer)) {
    if (-not (Test-Path -LiteralPath $exe)) {
        throw "Nuntius executable not found at $exe. Build it first with: cargo build -p ccc-cli"
    }

    Start-Process `
        -FilePath $exe `
        -ArgumentList @("serve") `
        -WorkingDirectory $project `
        -WindowStyle Hidden `
        -RedirectStandardOutput $outLog `
        -RedirectStandardError $errLog
}

$deadline = (Get-Date).AddSeconds(20)
do {
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $url -TimeoutSec 2 | Out-Null
        break
    } catch {
        Start-Sleep -Milliseconds 500
    }
} while ((Get-Date) -lt $deadline)

Start-Process $url
