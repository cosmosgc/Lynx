@echo off
setlocal

rem Apply this repository's PostgreSQL migrations using the configured .env.
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\db-migrate.ps1" %*
set "MIGRATION_EXIT_CODE=%ERRORLEVEL%"

echo.
if "%MIGRATION_EXIT_CODE%"=="0" (
    echo Database migration command completed successfully.
) else (
    echo Database migration command failed with exit code %MIGRATION_EXIT_CODE%.
)
pause
exit /b %MIGRATION_EXIT_CODE%
