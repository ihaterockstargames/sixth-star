@echo off
rem A/B benchmark of your own runs: see scripts\ab.ps1 for the options.
rem   ab.bat                      baseline, best, bestlate, hot1024, cardbuf1, 5 minutes each
rem   ab.bat -Configs baseline,hot512,hot2048 -Seconds 420
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\ab.ps1" %*
set "RC=%ERRORLEVEL%"
echo(%CMDCMDLINE%| findstr /i /c:" /c " >nul && pause
exit /b %RC%
