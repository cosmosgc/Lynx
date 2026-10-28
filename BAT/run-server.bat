@echo off
rem Run the local development world service. The token prompt is not hidden.
setlocal DisableDelayedExpansion
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

echo Lynx local development server - 127.0.0.1:8090
echo The token you type will be visible. Do not use a production credential.
echo Use the same token each time to retain the same demo character mapping.
set "LYNX_DEMO_TOKEN="
set /p "LYNX_DEMO_TOKEN=Demo token: "
if not defined LYNX_DEMO_TOKEN (
    echo A non-empty token is required.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)

rem Compute the hash from the environment without passing the token as an argument.
set "LYNX_WORLD_TOKEN_HASHES="
for /f "usebackq delims=" %%H in (`powershell -NoProfile -NonInteractive -Command "$sha = [System.Security.Cryptography.SHA256]::Create(); try { $bytes = [Text.Encoding]::UTF8.GetBytes($env:LYNX_DEMO_TOKEN); ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }"`) do set "LYNX_WORLD_TOKEN_HASHES=%%H"
set "LYNX_DEMO_TOKEN="
if not defined LYNX_WORLD_TOKEN_HASHES (
    echo Could not calculate the token hash. Check that PowerShell is available.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)

if not exist ".lynx" mkdir ".lynx"
if not exist ".lynx" (
    echo Could not create the local runtime directory.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)
echo.
echo Default addresses ^(if --bind is not overridden^):
echo Health:    http://127.0.0.1:8090/healthz
echo Readiness: http://127.0.0.1:8090/readyz
echo World:     ws://127.0.0.1:8090/world ^(WebSocket; bearer token required^)
echo.
echo Logs appear below. This window does not accept server commands.
echo Open another terminal to query endpoints; press Ctrl+C here to stop.
cargo run -p lynx-world -- serve --bind 127.0.0.1:8090 --state-file .lynx\world.json %*
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
