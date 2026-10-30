@echo off
rem Run the local Nakama-authenticated world service.
setlocal DisableDelayedExpansion
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

set "LYNX_MULTIPLAYER="
if /i "%~1"=="--multiplayer" set "LYNX_MULTIPLAYER=1"

echo Lynx local world server - 127.0.0.1:8090
echo Authentication: Nakama email account; character API: PostgreSQL.
echo Settings are loaded from the repository-root .env file.
if defined LYNX_MULTIPLAYER echo Using separate .lynx\multiplayer-world.json snapshot.
if not defined LYNX_MULTIPLAYER echo Using .lynx\world.json snapshot.

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
echo World:     ws://127.0.0.1:8090/world ^(Nakama bearer required^)
echo.
echo Requires LYNX_NAKAMA_URL and LYNX_PG_* settings in .env.
echo Logs appear below. Press Ctrl+C here to stop.

if defined LYNX_MULTIPLAYER goto multiplayer
cargo run -p lynx-world -- serve --auth-mode nakama --bind 127.0.0.1:8090 --state-file .lynx\world.json %*
goto finished

:multiplayer
if not "%~2"=="" (
    echo --multiplayer does not accept additional arguments.
    popd
    call "%~dp0_pause.bat"
    exit /b 2
)
cargo run -p lynx-world -- serve --auth-mode nakama --bind 127.0.0.1:8090 --state-file .lynx\multiplayer-world.json

:finished
set "RESULT=%ERRORLEVEL%"
popd
call "%~dp0_pause.bat"
exit /b %RESULT%
