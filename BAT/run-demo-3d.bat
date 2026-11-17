@echo off
rem Start the local 3D demo from any working directory.
setlocal DisableDelayedExpansion
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0..\scripts\demo-3d.ps1" %*
set "RESULT=%ERRORLEVEL%"
call "%~dp0_pause.bat"
exit /b %RESULT%
