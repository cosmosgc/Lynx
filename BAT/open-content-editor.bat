@echo off
rem Create the MMORPG editor demo project once, then open it in the editor.
setlocal DisableDelayedExpansion

pushd "%~dp0.."
if errorlevel 1 (
    echo Could not locate the Lynx repository.
    pause
    exit /b 1
)

set "PROJECT_DIR=%CD%\content-editor-demo"
if exist "%PROJECT_DIR%\" (
    if not exist "%PROJECT_DIR%\content\maps\overworld.ron" (
        echo The project folder exists but does not look like the generated demo:
        echo "%PROJECT_DIR%"
        echo Move it aside or choose another folder; this launcher will not overwrite it.
        set "RESULT=1"
        goto :done
    )
) else (
    echo Creating the MMORPG editor demo project...
    cargo run -p project_cli --bin lynx -- new content-editor-demo --template mmorpg --output "%PROJECT_DIR%"
    if errorlevel 1 (
        set "RESULT=1"
        goto :done
    )
)

echo Opening the content editor...
cargo run -p lynx_editor -- "%PROJECT_DIR%"
set "RESULT=%ERRORLEVEL%"

:done
popd
if not "%RESULT%"=="0" echo The content editor exited with code %RESULT%.
pause
exit /b %RESULT%
