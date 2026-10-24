@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

cargo clippy --workspace --all-targets -- -D warnings
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
