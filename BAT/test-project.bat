@echo off
setlocal
if "%~1"=="" (
    echo Usage: %~nx0 ^<path-to-Cargo.toml^> [cargo test options]
    call "%~dp0_pause.bat"
    exit /b 2
)

pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

cargo test --manifest-path "%~1" %2 %3 %4 %5 %6 %7 %8 %9
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
