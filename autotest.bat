@echo off
rem The autonomous test runner: see scripts\autotest.ps1 for the stages and options.
rem   autotest.bat                              one cycle: build, tests, benchmarks, title runs, comparison
rem   autotest.bat -Cycles 0 -PauseMinutes 30   cycles until stopped
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\autotest.ps1" %*
set "RC=%ERRORLEVEL%"
echo(%CMDCMDLINE%| findstr /i /c:" /c " >nul && pause
exit /b %RC%
