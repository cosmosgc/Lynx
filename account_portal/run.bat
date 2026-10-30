@echo off
setlocal
cd /d "%~dp0"

if not exist ".env" (
    copy ".env.example" ".env" >nul
    echo Created account_portal\.env from the example.
    echo Edit .env and replace FLASK_SECRET_KEY before running the portal.
    echo PORT controls the web app port; the default is 5050.
    pause
    exit /b 1
)

if not exist ".venv\Scripts\python.exe" (
    where py >nul 2>nul
    if not errorlevel 1 (
        py -3 -m venv .venv
    ) else (
        where python >nul 2>nul
        if errorlevel 1 (
            echo Python 3 was not found. Install Python and try again.
            pause
            exit /b 1
        )
        python -m venv .venv
    )
    if errorlevel 1 (
        echo Could not create the virtual environment.
        pause
        exit /b 1
    )
)

".venv\Scripts\python.exe" -c "import flask, dotenv, requests" >nul 2>nul
if errorlevel 1 (
    ".venv\Scripts\python.exe" -m pip install -r requirements.txt
    if errorlevel 1 (
        echo Dependency installation failed.
        pause
        exit /b 1
    )
)

".venv\Scripts\python.exe" app.py
if errorlevel 1 (
    echo Account portal exited with an error.
    pause
    exit /b 1
)

endlocal
