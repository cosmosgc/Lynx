$ErrorActionPreference = 'Stop'

$bytes = New-Object byte[] 32
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
$sha = [Security.Cryptography.SHA256]::Create()
$tokenPath = Join-Path $PSScriptRoot 'demo-token.txt'

try {
    $random.GetBytes($bytes)
    $token = [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+', '-').Replace('/', '_')
    $hash = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($token)))).Replace('-', '').ToLowerInvariant()
    [System.IO.File]::WriteAllText($tokenPath, $token, [System.Text.Encoding]::ASCII)
    $env:LYNX_WORLD_TOKEN_HASHES = $hash
}
finally {
    $random.Dispose()
    $sha.Dispose()
}

& cargo run -p lynx-world -- serve --bind 127.0.0.1:8090
exit $LASTEXITCODE
