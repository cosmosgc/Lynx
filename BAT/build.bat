@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo build --workspace %*
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
