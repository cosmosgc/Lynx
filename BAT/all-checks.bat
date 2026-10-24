@echo off
setlocal
set "LYNX_NO_PAUSE=1"

call "%~dp0format-check.bat"
if errorlevel 1 goto :failed

call "%~dp0clippy.bat"
if errorlevel 1 goto :failed

call "%~dp0test.bat"
if errorlevel 1 goto :failed

call "%~dp0build.bat"
if errorlevel 1 goto :failed

echo All Lynx checks passed.
set "LYNX_NO_PAUSE="
call "%~dp0_pause.bat"
exit /b 0

:failed
set "RESULT=%ERRORLEVEL%"
echo Lynx checks failed with exit code %RESULT%.
set "LYNX_NO_PAUSE="
call "%~dp0_pause.bat"
exit /b %RESULT%
