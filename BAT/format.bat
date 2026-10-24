@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo fmt --all
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
