@echo off
rem ============================================================================
rem Nuntius launcher - no PowerShell required.
rem Double-click (or use the "Launch Nuntius" shortcut): starts the server
rem if it isn't already running, waits until it responds, then opens the
rem admin console in your default browser.
rem
rem The server runs in a minimized console window titled "Nuntius Server".
rem Closing that window stops the station.
rem
rem System tools are called by full path so this works even when PATH is
rem broken, and the sleep uses ping (timeout.exe fails when stdin is
rem redirected, e.g. when run from other scripts).
rem ============================================================================
setlocal
set "PROJECT=%~dp0"
set "URL=http://127.0.0.1:3000/admin"
set "SYS=%SystemRoot%\System32"

rem Prefer the release build when it exists; fall back to debug.
set "EXE=%PROJECT%target\release\ccc-cli.exe"
if not exist "%EXE%" set "EXE=%PROJECT%target\debug\ccc-cli.exe"

rem Already running? Skip straight to opening the browser.
"%SYS%\tasklist.exe" /FI "IMAGENAME eq ccc-cli.exe" 2>nul | "%SYS%\find.exe" /I "ccc-cli.exe" >nul
if not errorlevel 1 goto :wait

if not exist "%EXE%" (
  echo Nuntius executable not found at:
  echo   %EXE%
  echo.
  echo Build it first with:  cargo build -p ccc-cli
  pause
  exit /b 1
)

cd /d "%PROJECT%"
start "Nuntius Server" /MIN cmd /c ""%EXE%" serve > "%PROJECT%nuntius-server.out.log" 2> "%PROJECT%nuntius-server.err.log""

:wait
rem Poll the server for up to ~40 seconds. curl.exe ships with Windows 10
rem 1803+; if it's missing, fall back to a fixed 3-second wait.
if not exist "%SYS%\curl.exe" (
  "%SYS%\ping.exe" -n 4 127.0.0.1 >nul
  goto :open
)
set /a TRIES=0
:poll
"%SYS%\curl.exe" -s -o nul -m 2 "%URL%" && goto :open
set /a TRIES+=1
if %TRIES% geq 40 goto :open
"%SYS%\ping.exe" -n 2 127.0.0.1 >nul
goto :poll

:open
start "" "%URL%"
endlocal
exit /b 0
