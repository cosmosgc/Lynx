@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo install --path tools/project_cli %*
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
