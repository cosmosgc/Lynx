#requires -Version 5.1
<#
.SYNOPSIS
    Start Nakama 3.41.0 against the native PostgreSQL database.

.DESCRIPTION
    The checked-in server/nakama/local.yml holds non-secret development
    settings. This script renders a gitignored overlay in .lynx/ with the
    database address, console credentials, server key, and log level from the
    environment, then starts the pinned Nakama binary.

    The database address is built from LYNX_PG_* and must already exist; run
    scripts/setup-postgres.ps1 and scripts/db-migrate.ps1 first.

.PARAMETER Check
    Validate the binary, configuration, and environment without starting Nakama
    or writing the secret overlay.

.EXAMPLE
    ./scripts/run-nakama.ps1

.EXAMPLE
    ./scripts/run-nakama.ps1 -Check
#>
[CmdletBinding()]
param(
    [switch]$Check
)

. (Join-Path $PSScriptRoot 'common.ps1')

$root = Get-LynxRepoRoot
Import-LynxEnv -Root $root
$pg = Get-LynxPgConfig

$binaryName = Get-LynxSetting 'NAKAMA_BIN' 'nakama'
$binary = Get-Command $binaryName -ErrorAction SilentlyContinue
if (-not $binary -or $binary.CommandType -ne 'Application') {
    throw "Nakama binary '$binaryName' was not found. Download Nakama 3.41.0 from https://github.com/heroiclabs/nakama/releases/tag/v3.41.0 and put it on PATH, or set NAKAMA_BIN."
}

$configPath = Join-Path $root 'server/nakama/local.yml'
if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
    throw "Nakama configuration not found: $configPath"
}

$consoleUser = Get-LynxSetting 'LYNX_NAKAMA_CONSOLE_USER' ''
$consolePassword = Get-LynxSetting 'LYNX_NAKAMA_CONSOLE_PASSWORD' ''
$serverKey = Get-LynxSetting 'LYNX_NAKAMA_SERVER_KEY' ''
$logLevel = Get-LynxSetting 'LYNX_NAKAMA_LOG_LEVEL' ''

if ($consoleUser -and $consoleUser -notmatch '^[A-Za-z0-9_.-]{3,32}$') {
    throw 'LYNX_NAKAMA_CONSOLE_USER must be 3-32 ASCII letters, digits, dots, dashes, or underscores.'
}
if ($consolePassword -and ($consolePassword -match '[\x00-\x1f\x7f]' -or $consolePassword.Length -gt 256)) {
    throw 'LYNX_NAKAMA_CONSOLE_PASSWORD must be at most 256 characters without control characters.'
}
if ($logLevel -and $logLevel -notmatch '^(debug|info|warn|error)$') {
    throw 'LYNX_NAKAMA_LOG_LEVEL must be one of: debug, info, warn, error.'
}
if ($serverKey -and ($serverKey -match '[\x00-\x1f\x7f]' -or $serverKey.Length -gt 256)) {
    throw 'LYNX_NAKAMA_SERVER_KEY must be at most 256 characters without control characters.'
}

$encodedUser = [Uri]::EscapeDataString($pg.User)
$encodedPassword = [Uri]::EscapeDataString($pg.Password)
$databaseAddress = "$encodedUser`:$encodedPassword@$($pg.Host):$($pg.Port)/$($pg.Database)"
$redactedAddress = "$($pg.User):***@$($pg.Host):$($pg.Port)/$($pg.Database)"

$overlayDirectory = Join-Path $root '.lynx'
$overlayPath = Join-Path $overlayDirectory 'nakama.local.yml'

if ($Check) {
    Write-Host 'Nakama preflight'
    Write-Host "  binary:   $($binary.Source)"
    Write-Host "  config:   $configPath"
    Write-Host "  database: $redactedAddress"
    Write-Host "  overlay:  $overlayPath (not written in -Check mode)"
    if (-not $consoleUser -or -not $consolePassword) {
        Write-Warning 'The console falls back to insecure defaults for missing credentials. Set LYNX_NAKAMA_CONSOLE_USER and LYNX_NAKAMA_CONSOLE_PASSWORD for anything but a local throwaway database.'
    }
    Write-Host 'Preflight OK.'
    exit 0
}

function ConvertTo-YamlSingleQuoted {
    param([Parameter(Mandatory = $true)][string]$Value)
    return "'" + $Value.Replace("'", "''") + "'"
}

$lines = New-Object System.Collections.Generic.List[string]
$lines.Add('database:')
$lines.Add('  address:')
$lines.Add('    - ' + (ConvertTo-YamlSingleQuoted $databaseAddress))

if ($serverKey) {
    $lines.Add('socket:')
    $lines.Add('  server_key: ' + (ConvertTo-YamlSingleQuoted $serverKey))
}

if ($consoleUser -or $consolePassword) {
    $lines.Add('console:')
    if ($consoleUser) {
        $lines.Add('  username: ' + (ConvertTo-YamlSingleQuoted $consoleUser))
    }
    if ($consolePassword) {
        $lines.Add('  password: ' + (ConvertTo-YamlSingleQuoted $consolePassword))
    }
}

if ($logLevel) {
    $lines.Add('logger:')
    $lines.Add('  level: ' + (ConvertTo-YamlSingleQuoted $logLevel))
}

if (-not (Test-Path -LiteralPath $overlayDirectory -PathType Container)) {
    New-Item -ItemType Directory -Path $overlayDirectory | Out-Null
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText($overlayPath, ($lines -join "`n") + "`n", $utf8NoBom)

Write-Host 'Starting Nakama 3.41.0'
Write-Host "  binary:   $($binary.Source)"
Write-Host "  config:   $configPath"
Write-Host "  overlay:  $overlayPath"
Write-Host "  database: $redactedAddress"
Write-Host "  console:  http://127.0.0.1:7351"
if (-not $consoleUser -or -not $consolePassword) {
    Write-Warning 'The console uses insecure defaults for missing credentials. Set LYNX_NAKAMA_CONSOLE_* for anything but a local throwaway database.'
}
Write-Host ''

Set-Location -LiteralPath $root
& $binary.Source '--config' $configPath '--config' $overlayPath
$code = $LASTEXITCODE

if ($code -ne 0) {
    Write-Host ''
    Write-Host "Nakama exited with code $code." -ForegroundColor Red
    Write-Host 'Check LYNX_PG_* values, the database existence, and that port 7350/7351 is free.'
}
exit $code
