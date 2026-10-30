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
if /i "%~1"=="--account" goto account
if not "%~1"=="" goto invalid

:account
if not "%~2"=="" goto invalid
echo Lynx account client
echo Start BAT\run-server.bat first; configure Nakama and PostgreSQL in the repository-root .env.
echo Sign in with your Nakama email account, then list, create, or select characters.
echo Password input is masked; credentials are kept in the operating-system credential store.
cargo run -p lynx_client -- --account
goto finished

:offline
if not "%~2"=="" goto invalid
echo Lynx offline placeholder
echo WASD moves the placeholder. No server connection is used.
cargo run -p lynx_client
goto finished

:help
echo Usage: BAT\run-client.bat [--account ^| --offline ^| --help]
echo Default: open the Bevy account and character-management screen.
echo --offline: launch the standalone placeholder without a server.
popd
call "%~dp0_pause.bat"
exit /b 0

:invalid
echo Usage: BAT\run-client.bat [--account ^| --offline ^| --help]
popd
call "%~dp0_pause.bat"
exit /b 2

:finished
set "RESULT=%ERRORLEVEL%"
popd
call "%~dp0_pause.bat"
exit /b %RESULT%
