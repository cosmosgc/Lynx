@echo off
setlocal
pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

if "%~1"=="" (
    cargo run -p project_cli -- doctor --project "%CD%"
) else (
    cargo run -p project_cli -- doctor %*
)
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
