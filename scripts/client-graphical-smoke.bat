@echo off
setlocal

cd /d "%~dp0.."
if errorlevel 1 (
    echo Failed to enter the Lynx repository root.
    exit /b 1
)

echo Building the windowed Lynx client...
cargo build -p lynx_client --features window
if errorlevel 1 (
    echo Client build failed.
    exit /b 1
)

echo Running the account-screen graphical smoke...
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0client-graphical-smoke.ps1" -ClientPath "%CD%\target\debug\lynx_client.exe"
exit /b %errorlevel%
