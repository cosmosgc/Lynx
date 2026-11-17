param(
    [switch]$Check,
    [ValidateSet('demo', 'debug', 'release')]
    [string]$Profile = 'demo',
    [ValidateRange(0, 120)]
    [int]$BenchmarkSeconds = 0
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$serverExe = Join-Path $root "target/$Profile/lynx-world.exe"
$clientExe = Join-Path $root "target/$Profile/lynx_client.exe"
if (!$Check) {
    Push-Location $root
    try {
        $cargoProfile = if ($Profile -eq 'debug') { 'dev' } else { $Profile }
        & cargo build --profile $cargoProfile -p lynx-world -p lynx_client
        if ($LASTEXITCODE -ne 0) { throw 'Could not build the demo binaries.' }
    } finally { Pop-Location }
}
if (!(Test-Path -LiteralPath $serverExe) -or !(Test-Path -LiteralPath $clientExe)) {
    throw "Demo binaries are missing for profile $Profile. Run the launcher without -Check to build them."
}

$ports = 18090, 18091
foreach ($port in $ports) {
    if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
        throw "Port $port is already in use. Stop the other service before starting the demo."
    }
}

# Each launch uses a disposable credential, held only in process environments.
$token = [Guid]::NewGuid().ToString('N') + [Guid]::NewGuid().ToString('N')
$sha = [System.Security.Cryptography.SHA256]::Create()
try {
    $tokenHash = -join ($sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($token)) | ForEach-Object { $_.ToString('x2') })
} finally {
    $sha.Dispose()
}
$names = 'LYNX_WORLD_TOKEN_HASHES', 'LYNX_WORLD_TOKEN_HASHES_FILE', 'LYNX_DEMO_TOKEN', 'LYNX_WORLD_URL', 'RUST_LOG', 'LYNX_CLIENT_BENCHMARK_SECONDS'
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$server = $null
Push-Location $root
try {
    $env:LYNX_WORLD_TOKEN_HASHES = $tokenHash
    [Environment]::SetEnvironmentVariable('LYNX_CLIENT_BENCHMARK_SECONDS', $null, 'Process')
    [Environment]::SetEnvironmentVariable('LYNX_WORLD_TOKEN_HASHES_FILE', $null, 'Process')
    $env:LYNX_WORLD_URL = 'ws://127.0.0.1:18090/world'
    $server = Start-Process -FilePath $serverExe -ArgumentList @('serve', '--auth-mode', 'demo', '--bind', '127.0.0.1:18090', '--admin-bind', '127.0.0.1:18091') -WorkingDirectory $env:TEMP -WindowStyle Hidden -PassThru
    $env:LYNX_DEMO_TOKEN = $token
    $ready = $false
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        if ($server.HasExited) { throw "Demo server exited with code $($server.ExitCode)." }
        try {
            $response = Invoke-WebRequest -Uri 'http://127.0.0.1:18090/readyz' -UseBasicParsing -TimeoutSec 1
            if ($response.StatusCode -eq 200) { $ready = $true; break }
        } catch { Start-Sleep -Milliseconds 200 }
    }
    if (!$ready) { throw 'Demo server did not become ready on 127.0.0.1:18090.' }
    if ($Check) {
        Write-Host 'Demo server is ready and the client executable is present.'
    } else {
        Write-Host "Starting the 3D demo ($Profile profile). WASD moves, Tab selects, number keys interact, and Escape opens options."
        if ($BenchmarkSeconds -gt 0) { $env:LYNX_CLIENT_BENCHMARK_SECONDS = $BenchmarkSeconds.ToString() }
        $env:RUST_LOG = 'info,wgpu=warn,wgpu_core=warn'
        $stdoutLog = Join-Path $env:TEMP "lynx-3d-client-$PID.stdout.log"
        $stderrLog = Join-Path $env:TEMP "lynx-3d-client-$PID.stderr.log"
        $client = Start-Process -FilePath $clientExe -ArgumentList '--demo-online' -WorkingDirectory $root -PassThru -Wait -NoNewWindow -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
        $renderErrors = @()
        foreach ($path in $stdoutLog, $stderrLog) {
            if (Test-Path -LiteralPath $path) {
                $renderErrors += Get-Content -LiteralPath $path | Select-String -Pattern 'error|warn|shader|pipeline|wgpu|FRAME_BENCHMARK' -CaseSensitive:$false | ForEach-Object { $_.Line }
            }
        }
        if ($renderErrors.Count -gt 0) {
            Write-Host "Renderer diagnostics (logs: $stdoutLog, $stderrLog):"
            $renderErrors | Select-Object -Last 80 | ForEach-Object { Write-Host $_ }
        }
        if ($client.ExitCode -ne 0) { throw "Demo client exited with code $($client.ExitCode)." }
    }
} finally {
    if ($server -and !$server.HasExited) { Stop-Process -Id $server.Id }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    Pop-Location
}
