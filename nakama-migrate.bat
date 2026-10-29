@echo off
setlocal

set "NAKAMA_BIN_DIR=D:\Development\MMORPG\Nakama"
if not exist "%NAKAMA_BIN_DIR%\nakama.exe" (
    echo Could not find "%NAKAMA_BIN_DIR%\nakama.exe".
    goto :failed_to_start
)

rem Add Nakama to PATH for this process and its children only.
set "PATH=%NAKAMA_BIN_DIR%;%PATH%"

rem Reuse the project setup script so migration uses the same database settings
rem and protected local config overlay as the Nakama server.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\run-nakama.ps1" -Migrate
set "MIGRATION_EXIT_CODE=%ERRORLEVEL%"

echo.
if "%MIGRATION_EXIT_CODE%"=="0" (
    echo Nakama database migrations completed successfully.
) else (
    echo Nakama database migrations failed with exit code %MIGRATION_EXIT_CODE%.
)
pause
exit /b %MIGRATION_EXIT_CODE%

:failed_to_start
pause
exit /b 1
