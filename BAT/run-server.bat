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
echo Use distinct tokens for simultaneous clients, in the same order on restart.
set "LYNX_MULTIPLAYER="
if /i "%~1"=="--multiplayer" set "LYNX_MULTIPLAYER=1"
if defined LYNX_MULTIPLAYER echo Using separate .lynx\multiplayer-world.json snapshot.
if not defined LYNX_MULTIPLAYER echo Existing snapshots do not automatically add characters for new tokens.
set "LYNX_WORLD_TOKEN_HASHES="
set "LYNX_DEMO_TOKEN="
set /p "LYNX_DEMO_TOKEN=First demo token: "
if not defined LYNX_DEMO_TOKEN (
    echo A non-empty token is required.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)

call :add_token
set "LYNX_DEMO_TOKEN="
if errorlevel 1 (
    echo Could not calculate the token hash. Check that PowerShell is available.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)

:more_tokens
set "LYNX_DEMO_TOKEN="
set /p "LYNX_DEMO_TOKEN=Another demo token (Enter to start): "
if not defined LYNX_DEMO_TOKEN goto tokens_done
call :add_token
set "LYNX_DEMO_TOKEN="
if errorlevel 1 (
    echo Could not calculate the token hash. Check that PowerShell is available.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)
goto more_tokens

:tokens_done
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
if defined LYNX_MULTIPLAYER goto multiplayer
cargo run -p lynx-world -- serve --bind 127.0.0.1:8090 --state-file .lynx\world.json %*
goto finished

:multiplayer
if not "%~2"=="" (
    echo --multiplayer does not accept additional arguments.
    popd
    call "%~dp0_pause.bat"
    exit /b 2
)
cargo run -p lynx-world -- serve --bind 127.0.0.1:8090 --state-file .lynx\multiplayer-world.json

:finished
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%

:add_token
rem Hash the token from the environment without passing it as an argument.
set "LYNX_TOKEN_HASH="
for /f "usebackq delims=" %%H in (`powershell -NoProfile -NonInteractive -Command "$sha = [System.Security.Cryptography.SHA256]::Create(); try { $bytes = [Text.Encoding]::UTF8.GetBytes($env:LYNX_DEMO_TOKEN); ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant() } finally { $sha.Dispose() }"`) do set "LYNX_TOKEN_HASH=%%H"
if not defined LYNX_TOKEN_HASH exit /b 1
if defined LYNX_WORLD_TOKEN_HASHES goto append_hash
set "LYNX_WORLD_TOKEN_HASHES=%LYNX_TOKEN_HASH%"
exit /b 0
:append_hash
set "LYNX_WORLD_TOKEN_HASHES=%LYNX_WORLD_TOKEN_HASHES%,%LYNX_TOKEN_HASH%"
exit /b 0
