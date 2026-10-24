@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo run -p project_cli -- doctor --backend %*
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
