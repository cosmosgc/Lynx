@echo off
setlocal
if "%~1"=="" (
    echo Usage: %~nx0 ^<name^> [--template minimal^|mmorpg] [--output path]
    exit /b 2
)

pushd "%~dp0.."
if errorlevel 1 exit /b 1

cargo run -p project_cli -- new %*
set "RESULT=%ERRORLEVEL%"

popd
exit /b %RESULT%
