@echo off
setlocal
if "%~1"=="" (
    echo Usage: %~nx0 ^<file-path^>
    exit /b 2
)

pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo run -p project_cli -- content hash "%~1" %2 %3 %4 %5 %6 %7 %8 %9
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
