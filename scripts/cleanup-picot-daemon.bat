@echo off
REM ABOUTME: Double-click entry point for the Windows daemon cleanup helper.
REM ABOUTME: Runs cleanup-picot-daemon.ps1 with any arguments and pauses when run interactively.

setlocal
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0cleanup-picot-daemon.ps1" %*
set "status=%ERRORLEVEL%"

REM Double-clicked from Explorer the window would close before the result can
REM be read; keep it open only when this was started without arguments.
if "%~1"=="" pause

exit /b %status%
