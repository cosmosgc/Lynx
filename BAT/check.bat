@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

cargo check --workspace --all-targets %*
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
