@echo off
rem Create a Lynx project from a selected template.
setlocal
if "%~1"=="" (
    echo Usage: %~nx0 ^<name^> [--template minimal^|mmorpg] [--output path]
    call "%~dp0_pause.bat"
    exit /b 2
)

pushd "%~dp0.."
if errorlevel 1 (
    call "%~dp0_pause.bat"
    exit /b 1
)

cargo run -p project_cli -- new %*
set "RESULT=%ERRORLEVEL%"

popd
call "%~dp0_pause.bat"
exit /b %RESULT%
