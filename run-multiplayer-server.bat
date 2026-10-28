@echo off
call "%~dp0BAT\run-server.bat" --multiplayer
exit /b %ERRORLEVEL%
