$ErrorActionPreference = 'Stop'

$tokenPath = Join-Path $PSScriptRoot 'demo-token.txt'
if (-not (Test-Path -LiteralPath $tokenPath -PathType Leaf)) {
    throw 'Demo token file not found. Start the demo server launcher first.'
}

$token = [System.IO.File]::ReadAllText($tokenPath).Trim()
if ($token -notmatch '^[A-Za-z0-9_-]{43}$') {
    throw 'Demo token file is invalid. Restart the demo server launcher to generate a new one.'
}

$env:LYNX_DEMO_TOKEN = $token
$env:LYNX_WORLD_URL = 'ws://127.0.0.1:8090/world'

& cargo run -p lynx_client -- --demo-online
exit $LASTEXITCODE
