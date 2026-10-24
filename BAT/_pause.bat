@echo off
rem Shared pause helper; skips the keypress when an aggregate script sets LYNX_NO_PAUSE.
if not defined LYNX_NO_PAUSE pause
exit /b 0
