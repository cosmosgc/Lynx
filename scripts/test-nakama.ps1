#requires -Version 5.1
<#
.SYNOPSIS
    Smoke-check local Nakama configuration and HTTP endpoints without starting it.

.PARAMETER NakamaBin
    Optional binary path/name used for the startup preflight.

.PARAMETER HealthUrl
    Nakama HTTP base URL (default http://127.0.0.1:7350).
#>
[CmdletBinding()]
param(
    [string]$NakamaBin,
    [string]$HealthUrl = 'http://127.0.0.1:7350'
)

. (Join-Path $PSScriptRoot 'common.ps1')
$root = Get-LynxRepoRoot
Import-LynxEnv -Root $root
if ($NakamaBin) {
    $env:NAKAMA_BIN = $NakamaBin
}

Write-Host 'Nakama smoke test (read-only; does not start or restart Nakama)'

# Reuse the regular startup validation, but in its no-write/no-start mode.
$runner = Join-Path $PSScriptRoot 'run-nakama.ps1'
$hostExe = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh.exe' } else { 'powershell.exe' }
$hostPath = Join-Path $PSHOME $hostExe
& $hostPath -NoProfile -File $runner -Check
if ($LASTEXITCODE -ne 0) {
    throw "Nakama preflight failed with exit code $LASTEXITCODE."
}

# Check explicitly configured keys against the gitignored overlay without
# ever printing their values.
$overlay = Join-Path $root '.lynx/nakama.local.yml'
if (-not (Test-Path -LiteralPath $overlay -PathType Leaf)) {
    throw "Nakama secret overlay not found: $overlay. Run scripts/run-nakama.ps1 once to render it (that starts Nakama)."
}
$overlayText = [IO.File]::ReadAllText($overlay)
$overlayKeys = @(
    @{ Name = 'LYNX_NAKAMA_SERVER_KEY'; YamlKey = 'server_key' },
    @{ Name = 'LYNX_NAKAMA_CONSOLE_SIGNING_KEY'; YamlKey = 'signing_key' },
    @{ Name = 'LYNX_NAKAMA_SESSION_ENCRYPTION_KEY'; YamlKey = 'encryption_key' },
    @{ Name = 'LYNX_NAKAMA_REFRESH_ENCRYPTION_KEY'; YamlKey = 'refresh_encryption_key' },
    @{ Name = 'LYNX_NAKAMA_RUNTIME_HTTP_KEY'; YamlKey = 'http_key' }
)
$checkedKeys = 0
foreach ($entry in $overlayKeys) {
    $value = Get-LynxSetting $entry.Name ''
    if (-not $value) { continue }
    $quotedValue = "'" + $value.Replace("'", "''") + "'"
    $line = '(?m)^  ' + [regex]::Escape($entry.YamlKey) + ': ' + [regex]::Escape($quotedValue) + '\s*$'
    if ($overlayText -notmatch $line) {
        throw "Configured $($entry.Name) does not match the generated overlay key '$($entry.YamlKey)'."
    }
    $checkedKeys++
}
Write-Host "PASS security overlay: $checkedKeys configured key(s) match (values hidden)"

$healthEndpoint = $HealthUrl.TrimEnd('/') + '/healthcheck'
try {
    $health = Invoke-WebRequest -Uri $healthEndpoint -UseBasicParsing -TimeoutSec 5
} catch {
    throw "Nakama health check failed at $healthEndpoint : $($_.Exception.Message)"
}
if ([int]$health.StatusCode -ne 200) {
    throw "Nakama health check returned HTTP $([int]$health.StatusCode), expected 200."
}
Write-Host "PASS health: GET $healthEndpoint returned HTTP 200"

$metricsPort = Get-LynxSetting 'LYNX_NAKAMA_METRICS_PORT' ''
if (-not $metricsPort -or $metricsPort -eq '0') {
    Write-Host 'SKIP metrics: LYNX_NAKAMA_METRICS_PORT is unset or 0 (metrics disabled)'
} else {
    $metricsEndpoint = "http://127.0.0.1:$metricsPort/metrics"
    try {
        $metrics = Invoke-WebRequest -Uri $metricsEndpoint -UseBasicParsing -TimeoutSec 5
    } catch {
        throw "Nakama metrics check failed at $metricsEndpoint : $($_.Exception.Message)"
    }
    if ([int]$metrics.StatusCode -ne 200 -or $metrics.Content -notmatch '(?m)^# (HELP|TYPE)\s') {
        throw "Nakama metrics endpoint $metricsEndpoint did not return HTTP 200 Prometheus text."
    }
    Write-Host "PASS metrics: GET $metricsEndpoint returned HTTP 200 Prometheus text"
}

Write-Host 'Nakama smoke test passed.'
