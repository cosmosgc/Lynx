@echo off
setlocal

call "%~dp0format-check.bat"
if errorlevel 1 goto :failed

call "%~dp0clippy.bat"
if errorlevel 1 goto :failed

call "%~dp0test.bat"
if errorlevel 1 goto :failed

call "%~dp0build.bat"
if errorlevel 1 goto :failed

echo All Lynx checks passed.
exit /b 0

:failed
set "RESULT=%ERRORLEVEL%"
echo Lynx checks failed with exit code %RESULT%.
exit /b %RESULT%
