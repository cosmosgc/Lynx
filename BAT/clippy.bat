@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo clippy --workspace --all-targets -- -D warnings
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
