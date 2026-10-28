@echo off
rem Run the graphical client from any working directory.
setlocal DisableDelayedExpansion
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

if /i "%~1"=="--help" goto help
if /i "%~1"=="--offline" goto offline
if not "%~1"=="" goto invalid

echo Lynx development client - online demo
echo Start BAT\run-server.bat first and use the same demo token.
echo The token you type will be visible. Do not use a production credential.
echo The online world view is read-only; movement and interactions are not yet available.
echo Default world: ws://127.0.0.1:8090/world
echo Set LYNX_DEMO_URL before launching to override the address.
set "LYNX_DEMO_TOKEN="
set /p "LYNX_DEMO_TOKEN=Demo token: "
if not defined LYNX_DEMO_TOKEN (
    echo A non-empty token is required.
    popd
    call "%~dp0_pause.bat"
    exit /b 1
)
cargo run -p lynx_client -- --demo-online
goto finished

:offline
if not "%~2"=="" goto invalid
echo Lynx development client - offline placeholder
echo WASD moves the placeholder. No server connection is used.
cargo run -p lynx_client
goto finished

:help
echo Usage: BAT\run-client.bat [--offline ^| --help]
echo Default: connect to the local demo server using a visible token prompt.
echo --offline: launch the standalone placeholder without a server.
popd
call "%~dp0_pause.bat"
exit /b 0

:invalid
echo Usage: BAT\run-client.bat [--offline ^| --help]
popd
call "%~dp0_pause.bat"
exit /b 2

:finished
set "RESULT=%ERRORLEVEL%"
set "LYNX_DEMO_TOKEN="
popd
call "%~dp0_pause.bat"
exit /b %RESULT%
