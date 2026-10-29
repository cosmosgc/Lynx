@echo off
setlocal

set "NAKAMA_BIN_DIR=D:\Development\MMORPG\Nakama"
if not exist "%NAKAMA_BIN_DIR%\nakama.exe" (
    echo Could not find "%NAKAMA_BIN_DIR%\nakama.exe".
    goto :failed_to_start
)

rem Add Nakama to PATH for this process and its children only.
set "PATH=%NAKAMA_BIN_DIR%;%PATH%"

rem Reuse the project script to load .env, render the local config overlay,
rem and start Nakama against the configured PostgreSQL database.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-nakama.ps1" %*
set "NAKAMA_EXIT_CODE=%ERRORLEVEL%"
echo.
echo Nakama stopped with exit code %NAKAMA_EXIT_CODE%.
pause
exit /b %NAKAMA_EXIT_CODE%

:failed_to_start
pause
exit /b 1
