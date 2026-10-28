@echo off
setlocal
pushd "%~dp0"
if errorlevel 1 (
    echo Could not open the dashboard directory.
    pause
    exit /b 1
)

python -c "import flask" >nul 2>nul
if errorlevel 1 (
    echo Flask is not installed for the current Python interpreter.
    echo Run: python -m pip install -r requirements.txt
    popd
    pause
    exit /b 1
)

echo Starting the Lynx backlog dashboard at http://127.0.0.1:5000
echo Press Ctrl+C to stop the server.
python app.py
set "RESULT=%ERRORLEVEL%"
popd
if not "%RESULT%"=="0" pause
exit /b %RESULT%
