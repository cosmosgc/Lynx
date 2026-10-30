@echo off
rem Start the Nakama-authenticated multiplayer world with its own snapshot.
call "%~dp0BAT\run-server.bat" --multiplayer
exit /b %ERRORLEVEL%
