@echo off
rem Validate content in a generated project directory and show JSON diagnostics.
setlocal
if "%~1"=="" (
    echo Usage: %~nx0 ^<project-directory^>
    call "%~dp0_pause.bat"
    exit /b 2
)

set "REPORT=%TEMP%\lynx-validate-%RANDOM%-%RANDOM%.json"
set "LYNX_VALIDATE_REPORT=%REPORT%"

pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

cargo run -p project_cli -- validate --project "%~1" --json > "%REPORT%"
set "RESULT=%ERRORLEVEL%"
if not "%RESULT%"=="0" goto :show_report

powershell -NoProfile -Command "$report = Get-Content -Raw -LiteralPath $env:LYNX_VALIDATE_REPORT | ConvertFrom-Json; if (-not $report.ok) { exit 1 }"
set "RESULT=%ERRORLEVEL%"

:show_report
if exist "%REPORT%" type "%REPORT%"
if exist "%REPORT%" del "%REPORT%" >nul 2>&1
popd
call "%~dp0_pause.bat"
exit /b %RESULT%
