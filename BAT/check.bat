@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo check --workspace --all-targets %*
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
