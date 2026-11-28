@echo off
setlocal EnableExtensions DisableDelayedExpansion
rem ===========================================================================
rem  Sixth Star launcher: one click builds what changed and runs your title.
rem
rem    run.bat              build (only what changed) + run the title
rem    run.bat nobuild      run the last build without building
rem    run.bat check        build + load and bind the title, do not start it
rem    run.bat pipelines    build + pre-build every pipeline once (SHADER_ARCHIVE)
rem    run.bat doctor       check settings and dependencies only
rem    run.bat rebar        report whether Resizable BAR is active
rem    run.bat selftest     the launch layout on synthetic files (no title needed)
rem    run.bat tests        the full test suite
rem    run.bat membench ^| vkbench ^| dxr-proto ^| vkrt [args]   GPU benchmarks
rem    run.bat help
rem
rem  Settings: run.config.bat next to this file (made from
rem  run.config.example.bat the first time), or the file SIXTH_CONFIG names.
rem  Logs: LOG_DIR (default logs\ next to the settings file), one per run.
rem  Caches and the title's data: CACHE_DIR and DATA_DIR (default
rem  %LOCALAPPDATA%\victory\cache and ...\data).
rem  The exit code is the title's (or the failing step's). NOPAUSE=1 never
rem  waits for a key; otherwise a double-clicked run.bat waits at the end.
rem ===========================================================================

set "REPO=%~dp0"
set "REPO=%REPO:~0,-1%"
set "BIN=%REPO%\target\release\sixth-star.exe"
set "TEE=%REPO%\scripts\tee-run.ps1"
set "MODE=%~1"
if "%MODE%"=="" set "MODE=run"
set "EXTRA=%2 %3 %4 %5 %6 %7 %8 %9"
set "RC=0"
set "PAUSE_AT_END="
echo(%CMDCMDLINE%| findstr /i /c:" /c " >nul && set "PAUSE_AT_END=1"
if "%NOPAUSE%"=="1" set "PAUSE_AT_END="
if exist "%USERPROFILE%\.cargo\bin" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

if /i "%MODE%"=="help" goto :help
if /i "%MODE%"=="-h" goto :help
if /i "%MODE%"=="/?" goto :help
for %%m in (run nobuild check pipelines doctor rebar selftest tests membench vkbench dxr-proto vkrt) do if /i "%MODE%"=="%%m" goto :known
echo [run] unknown command "%MODE%"
set "RC=2"
goto :help

:known
if /i "%MODE%"=="rebar" goto :rebar
rem ---- settings ---------------------------------------------------------------
if not defined SIXTH_CONFIG set "SIXTH_CONFIG=%REPO%\run.config.bat"
for %%I in ("%SIXTH_CONFIG%") do set "SIXTH_CONFIG=%%~fI"
for %%I in ("%SIXTH_CONFIG%") do set "CONFIG_DIR=%%~dpI"
set "CONFIG_DIR=%CONFIG_DIR:~0,-1%"
set "NEEDS_TITLE="
for %%m in (run nobuild check pipelines doctor) do if /i "%MODE%"=="%%m" set "NEEDS_TITLE=1"
if exist "%SIXTH_CONFIG%" goto :have_config
if not defined NEEDS_TITLE goto :config_done
if exist "%REPO%\run.config.example.bat" if /i "%SIXTH_CONFIG%"=="%REPO%\run.config.bat" copy /y "%REPO%\run.config.example.bat" "%SIXTH_CONFIG%" >nul
echo [run] no settings yet: fill in GAME_DIR and TITLE_EXE in
echo [run]   "%SIXTH_CONFIG%"
echo [run] then start run.bat again.
set "RC=1"
goto :end
:have_config
call "%SIXTH_CONFIG%"
:config_done

if not defined LOG_DIR set "LOG_DIR=logs"
call :abspath LOG_DIR "%LOG_DIR%" "%CONFIG_DIR%"
if not exist "%LOG_DIR%" mkdir "%LOG_DIR%" 2>nul
if not exist "%LOG_DIR%" (
    echo [run] cannot create the log folder "%LOG_DIR%"
    set "RC=1"
    goto :end
)
for /f %%t in ('powershell -NoProfile -Command "Get-Date -Format yyyyMMdd_HHmmss"') do set "STAMP=%%t"
set "LOG=%LOG_DIR%\%MODE%_%STAMP%.log"
set "SIXTH_LOG=%LOG%"
> "%LOG%" echo [run] Sixth Star %MODE% %STAMP%
call :say "settings  %SIXTH_CONFIG%"
call :say "log       %LOG%"

rem ---- dependencies ------------------------------------------------------------
set "HAVE_CARGO="
where cargo >nul 2>&1 && set "HAVE_CARGO=1"
if defined HAVE_CARGO goto :cargo_ok
if /i "%MODE%"=="nobuild" goto :cargo_ok
if /i "%MODE%"=="doctor" goto :cargo_ok
if exist "%BIN%" if defined NEEDS_TITLE (
    call :say "WARNING   no cargo on PATH: using the last build, not rebuilding"
    goto :cargo_ok
)
call :say "ERROR     no cargo on PATH. Install Rust from https://rustup.rs (with the"
call :say "          MSVC build tools), then start run.bat again."
set "RC=1"
goto :end
:cargo_ok
if defined HAVE_CARGO call :say "cargo     found"
if not defined HAVE_CARGO call :say "cargo     not found"
set "SIXTH_COMMIT="
for /f "delims=" %%c in ('git -C "%REPO%" rev-parse --short HEAD 2^>nul') do set "SIXTH_COMMIT=%%c"
if defined SIXTH_COMMIT call :say "commit    %SIXTH_COMMIT%"
set "DXIL=%VICTORY_DXIL%"
set "KITS=%ProgramFiles(x86)%\Windows Kits\10\bin"
if not defined DXIL for /d %%d in ("%KITS%\10.*") do if exist "%%d\x64\dxil.dll" set "DXIL=%%d\x64\dxil.dll"
if defined DXIL call :say "dxil.dll  %DXIL%"
if not defined DXIL call :say "WARNING   no dxil.dll (Windows SDK); only the shader checks in the tests need it"

if /i "%MODE%"=="selftest" goto :selftest
if /i "%MODE%"=="tests" goto :tests
for %%m in (membench vkbench dxr-proto vkrt) do if /i "%MODE%"=="%%m" goto :bench

rem ---- the title's layout --------------------------------------------------------
if not defined GAME_DIR (
    call :say "ERROR     GAME_DIR is not set in the settings file"
    set "RC=1"
    goto :end
)
if not defined TITLE_EXE (
    call :say "ERROR     TITLE_EXE is not set in the settings file"
    set "RC=1"
    goto :end
)
call :abspath GAME_DIR "%GAME_DIR%" "%CONFIG_DIR%"
call :abspath TITLE_PATH "%TITLE_EXE%" "%GAME_DIR%"
set "WORK_PATH="
if defined WORK_DIR call :abspath WORK_PATH "%WORK_DIR%" "%GAME_DIR%"
set "MANIFEST_PATH="
if defined MANIFEST call :abspath MANIFEST_PATH "%MANIFEST%" "%GAME_DIR%"
call :say "package   %GAME_DIR%"
call :say "title     %TITLE_PATH%"
if not exist "%GAME_DIR%\" (
    call :say "ERROR     GAME_DIR is not a folder"
    set "RC=1"
    goto :end
)
call :outside_repo "%GAME_DIR%" GAME_DIR || goto :end
if not exist "%TITLE_PATH%" (
    call :say "ERROR     the title executable does not exist"
    set "RC=1"
    goto :end
)
if exist "%TITLE_PATH%\" (
    call :say "ERROR     TITLE_EXE is a folder, not the executable"
    set "RC=1"
    goto :end
)
if defined WORK_PATH if not exist "%WORK_PATH%\" (
    call :say "ERROR     WORK_DIR is not a folder: %WORK_PATH%"
    set "RC=1"
    goto :end
)
if defined MANIFEST_PATH if not exist "%MANIFEST_PATH%" (
    call :say "ERROR     MANIFEST does not exist: %MANIFEST_PATH%"
    set "RC=1"
    goto :end
)
if not defined MANIFEST_PATH if not exist "%GAME_DIR%\MicrosoftGame.config" call :say "WARNING   no MicrosoftGame.config in GAME_DIR: the title gets id 0"

if "%CONSOLE%"=="" set "CONSOLE=series-x"
if /i "%CONSOLE%"=="series-x" goto :console_ok
if /i not "%CONSOLE%"=="series-s" (
    call :say "ERROR     CONSOLE must be series-x or series-s, not %CONSOLE%"
    set "RC=1"
    goto :end
)
rem series-s without the FidelityFX SDK: the title's upscale is answered
rem with a bilinear scale (VICTORY_FFX_SIMPLE=1, no FSR, no history).
if "%VICTORY_FFX_SIMPLE%"=="1" (
    call :say "console   series-s with VICTORY_FFX_SIMPLE=1: bilinear upscale, FFX_DIR not used"
    goto :console_ok
)
if not defined FFX_DIR (
    call :say "WARNING   series-s without FFX_DIR (FidelityFX signedbin): VICTORY_FFX_SIMPLE=1, a bilinear upscale instead of FSR"
    goto :ffx_simple
)
call :abspath FFX_DIR "%FFX_DIR%" "%CONFIG_DIR%"
if not exist "%FFX_DIR%\" (
    call :say "WARNING   FFX_DIR is not a folder: %FFX_DIR%; VICTORY_FFX_SIMPLE=1, a bilinear upscale instead of FSR"
    goto :ffx_simple
)
set "VICTORY_FFX_DIR=%FFX_DIR%"
goto :console_ok
:ffx_simple
set "VICTORY_FFX_SIMPLE=1"
:console_ok

rem Resolution: OUTPUT_MODE is the window and swap chain (empty: they follow
rem the title's image), RENDER_SCALE what the title renders at relative to
rem it, DISPLAY_MODE (if set) the mode the title is told, outright.
if defined DISPLAY_MODE echo(%DISPLAY_MODE%| findstr /i /r /c:"^[0-9][0-9]*x[0-9][0-9]*$" >nul || (
    call :say "WARNING   DISPLAY_MODE must be WxH or empty, not %DISPLAY_MODE%; left empty"
    set "DISPLAY_MODE="
)
if defined OUTPUT_MODE echo(%OUTPUT_MODE%| findstr /i /r /c:"^[0-9][0-9]*x[0-9][0-9]*$" >nul || (
    call :say "WARNING   OUTPUT_MODE must be WxH or empty, not %OUTPUT_MODE%; left empty"
    set "OUTPUT_MODE="
)
if "%RENDER_SCALE%"=="" set "RENDER_SCALE=1.0"
echo(%RENDER_SCALE%| findstr /r /c:"^[0-9]*\.*[0-9][0-9]*$" >nul || (
    call :say "WARNING   RENDER_SCALE must be a number from 0.1 to 2.0, not %RENDER_SCALE%; using 1.0"
    set "RENDER_SCALE=1.0"
)
set "VICTORY_DISPLAY_MODE="
if defined DISPLAY_MODE set "VICTORY_DISPLAY_MODE=%DISPLAY_MODE%"
set "VICTORY_OUTPUT_MODE="
if defined OUTPUT_MODE set "VICTORY_OUTPUT_MODE=%OUTPUT_MODE%"
set "VICTORY_RENDER_SCALE=%RENDER_SCALE%"
rem What the title is told, as crates\victory-gpu\src\resolution.rs computes
rem it (shown by doctor; the runner logs the exact value at start).
set "SHOW_DISPLAY=%DISPLAY_MODE%"
if not defined SHOW_DISPLAY if exist "%REPO%\scripts\display.ps1" powershell -NoProfile -ExecutionPolicy Bypass -File "%REPO%\scripts\display.ps1" "%OUTPUT_MODE%" "%RENDER_SCALE%" > "%TEMP%\sixth-star-display.txt" 2>nul
if not defined SHOW_DISPLAY if exist "%TEMP%\sixth-star-display.txt" set /p SHOW_DISPLAY=<"%TEMP%\sixth-star-display.txt"
if exist "%TEMP%\sixth-star-display.txt" del "%TEMP%\sixth-star-display.txt" 2>nul
if not defined SHOW_DISPLAY set "SHOW_DISPLAY=1920x1080 x %RENDER_SCALE%"
set "SHOW_OUTPUT=%OUTPUT_MODE%"
if not defined SHOW_OUTPUT set "SHOW_OUTPUT=follows the title's image"
if defined BCPACK_TABLES call :abspath BCPACK_TABLES "%BCPACK_TABLES%" "%CONFIG_DIR%"
if defined BCPACK_TABLES (
    if exist "%BCPACK_TABLES%" (
        set "VICTORY_BCPACK_TABLES=%BCPACK_TABLES%"
    ) else (
        call :say "WARNING   BCPACK_TABLES does not exist: %BCPACK_TABLES%"
        set "BCPACK_TABLES="
    )
)

if "%SHADER_OPT%"=="" set "SHADER_OPT=0"
set "VICTORY_SHADER_OPT=%SHADER_OPT%"
if "%DRED%"=="1" set "VICTORY_D3D12_DRED=1"
if defined SKIP_PIPELINES set "VICTORY_SKIP_PIPELINES=%SKIP_PIPELINES%"
if defined SKIP_LIST call :abspath SKIP_LIST "%SKIP_LIST%" "%CONFIG_DIR%"
if defined SKIP_LIST if not exist "%SKIP_LIST%" (
    call :say "WARNING   SKIP_LIST does not exist: %SKIP_LIST%"
    set "SKIP_LIST="
)
if defined SKIP_LIST for /f "usebackq delims=" %%s in ("%SKIP_LIST%") do set "VICTORY_SKIP_PIPELINES=%%s"
if /i "%PRESET%"=="klbf" set "PRESET=fast"
if /i "%PRESET%"=="fast" (
    set "VICTORY_LATE_WAIT=1"
    set "VICTORY_WIDE_BACK=1"
    set "VICTORY_PIPELINE_WAIT_MS=24"
    set "VICTORY_RELAY_COPY_QUEUE=0"
)
if not defined PRESET set "PRESET=safe"
if "%DRED%"=="" set "DRED=0"

rem Caches (translated shaders, mapped images) and the title's data (its
rem storage: saves; its temporary drive). Both stay outside the repository.
if defined CACHE_DIR call :abspath CACHE_DIR "%CACHE_DIR%" "%CONFIG_DIR%"
if defined CACHE_DIR set "VICTORY_CACHE_DIR=%CACHE_DIR%"
if defined DATA_DIR call :abspath DATA_DIR "%DATA_DIR%" "%CONFIG_DIR%"
if defined DATA_DIR set "VICTORY_DATA_DIR=%DATA_DIR%"
set "SHOW_CACHE=%VICTORY_CACHE_DIR%"
if not defined SHOW_CACHE set "SHOW_CACHE=%LOCALAPPDATA%\victory\cache"
set "SHOW_DATA=%VICTORY_DATA_DIR%"
if not defined SHOW_DATA set "SHOW_DATA=%LOCALAPPDATA%\victory\data"
call :outside_repo "%SHOW_CACHE%" CACHE_DIR || goto :end
call :outside_repo "%SHOW_DATA%" DATA_DIR || goto :end
if not exist "%SHOW_CACHE%\" mkdir "%SHOW_CACHE%" 2>nul
if not exist "%SHOW_DATA%\" mkdir "%SHOW_DATA%" 2>nul
if not exist "%SHOW_CACHE%\" (
    call :say "ERROR     cannot create the cache folder: %SHOW_CACHE%"
    set "RC=1"
    goto :end
)
if not exist "%SHOW_DATA%\" (
    call :say "ERROR     cannot create the data folder: %SHOW_DATA%"
    set "RC=1"
    goto :end
)
call :say "cache     %SHOW_CACHE%"
call :say "data      %SHOW_DATA%"
call :say "console   %CONSOLE%, preset %PRESET%, shader_opt %SHADER_OPT%, dred %DRED%"
call :say "display   %SHOW_DISPLAY%"
call :say "output    %SHOW_OUTPUT%"
call :say "scale     %RENDER_SCALE%"
if defined DISPLAY_MODE if not "%RENDER_SCALE%"=="1.0" if not "%RENDER_SCALE%"=="1" call :say "WARNING   DISPLAY_MODE is set, so RENDER_SCALE %RENDER_SCALE% is ignored (empty DISPLAY_MODE to use the scale)"
if not defined OUTPUT_MODE if not "%RENDER_SCALE%"=="1.0" if not "%RENDER_SCALE%"=="1" call :say "WARNING   RENDER_SCALE without OUTPUT_MODE: the window follows the smaller image (set OUTPUT_MODE to scale it up)"
call :say "dstorage  console HLE (CPU zlib/BCPack)"
set "SHOW_BCPACK="
if defined VICTORY_BCPACK_TABLES set "SHOW_BCPACK=%VICTORY_BCPACK_TABLES%"
if not defined SHOW_BCPACK if exist "%REPO%\crates\victory-formats\src\bcpack-tables.bin" set "SHOW_BCPACK=%REPO%\crates\victory-formats\src\bcpack-tables.bin"
if not defined SHOW_BCPACK if exist "%REPO%\target\release\bcpack-tables.bin" set "SHOW_BCPACK=%REPO%\target\release\bcpack-tables.bin"
if defined SHOW_BCPACK (
    call :say "bcpack    %SHOW_BCPACK%"
) else (
    call :say "bcpack    not found (BCPack textures fail; see README)"
)
if exist "%REPO%\scripts\rebar.ps1" powershell -NoProfile -ExecutionPolicy Bypass -File "%REPO%\scripts\rebar.ps1"
if defined VICTORY_SKIP_PIPELINES call :say "skipping  %VICTORY_SKIP_PIPELINES%"
if /i "%MODE%"=="doctor" (
    call :say "doctor    settings and dependencies look usable"
    goto :end
)

rem ---- build (cargo only rebuilds what changed) ----------------------------------
if /i "%MODE%"=="nobuild" goto :built
if not defined HAVE_CARGO goto :built
call :say "building  sixth-star (release; only what changed)"
set "SIXTH_PROGRAM=cargo"
set "SIXTH_ARGS=build|--release|-p|victory|--manifest-path|%REPO%\Cargo.toml"
call :tee
if not "%RC%"=="0" (
    call :say "ERROR     the build failed (see above and the log)"
    goto :end
)
:built
if not exist "%BIN%" (
    call :say "ERROR     no build yet: %BIN% (run run.bat without nobuild)"
    set "RC=1"
    goto :end
)

if /i "%MODE%"=="pipelines" goto :pipelines

rem ---- run ------------------------------------------------------------------------
set "SIXTH_PROGRAM=%BIN%"
set "SIXTH_ARGS=run|--console|%CONSOLE%|--package|%GAME_DIR%"
if defined WORK_PATH set "SIXTH_ARGS=%SIXTH_ARGS%|--working-dir|%WORK_PATH%"
if defined MANIFEST_PATH set "SIXTH_ARGS=%SIXTH_ARGS%|--manifest|%MANIFEST_PATH%"
if /i not "%MODE%"=="check" set "SIXTH_ARGS=%SIXTH_ARGS%|--execute"
set "SIXTH_ARGS=%SIXTH_ARGS%|%TITLE_PATH%"
if /i "%MODE%"=="check" (call :say "checking  load and bind only, the title is not started") else call :say "starting  the title; the frame line (fps, ms/frame) is in the log"
call :tee
call :say "finished  exit code %RC%"
goto :end

:pipelines
if not defined SHADER_ARCHIVE (
    call :say "ERROR     SHADER_ARCHIVE is not set (the title's shader archive)"
    set "RC=1"
    goto :end
)
call :abspath SHADER_ARCHIVE "%SHADER_ARCHIVE%" "%GAME_DIR%"
if not exist "%SHADER_ARCHIVE%" (
    call :say "ERROR     SHADER_ARCHIVE does not exist: %SHADER_ARCHIVE%"
    set "RC=1"
    goto :end
)
call :say "pipelines from %SHADER_ARCHIVE%"
set "SIXTH_PROGRAM=%BIN%"
set "SIXTH_ARGS=translate|--cache|--pipelines|--jobs|%NUMBER_OF_PROCESSORS%|%SHADER_ARCHIVE%"
call :tee
goto :end

rem ---- no title needed -------------------------------------------------------------
:selftest
call :say "selftest  launch layout on synthetic files only"
set "SIXTH_PROGRAM=cargo"
set "SIXTH_ARGS=test|--release|--manifest-path|%REPO%\Cargo.toml|-p|victory-loader|-p|victory|--test|title_dlls|--test|launch_layout"
call :tee
if not "%RC%"=="0" goto :selftest_done
set "SIXTH_ARGS=test|--release|--manifest-path|%REPO%\Cargo.toml|-p|victory-loader|--lib|layout"
call :tee
:selftest_done
if "%RC%"=="0" (call :say "selftest  passed") else call :say "selftest  FAILED (exit code %RC%)"
goto :end

:tests
if not defined SIXTH_REQUIRE_GPU set "SIXTH_REQUIRE_GPU=1"
call :say "tests     whole workspace (SIXTH_REQUIRE_GPU=%SIXTH_REQUIRE_GPU%)"
set "SIXTH_PROGRAM=cargo"
set "SIXTH_ARGS=test|--workspace|--release|--manifest-path|%REPO%\Cargo.toml"
call :tee
goto :end

:bench
set "CSV=%LOG_DIR%\%MODE%_%STAMP%.csv"
call :say "benchmark %MODE%, results in %CSV%"
set "SIXTH_PROGRAM=cargo"
set "SIXTH_ARGS=run|--release|--manifest-path|%REPO%\Cargo.toml|-p|%MODE%|--|--csv|%CSV%"
for %%a in (%EXTRA%) do call set "SIXTH_ARGS=%%SIXTH_ARGS%%|%%~a"
call :tee
goto :end

rem ---- helpers -----------------------------------------------------------------------
rem :abspath VAR "path" "base"   VAR = path, made absolute against base if relative
:abspath
setlocal
set "P=%~2"
set "R=%~3\%~2"
if "%P:~1,1%"==":" set "R=%P%"
if "%P:~0,2%"=="\\" set "R=%P%"
for %%I in ("%R%") do set "R=%%~fI"
if "%R:~-1%"=="\" if not "%R:~-2%"==":\" set "R=%R:~0,-1%"
endlocal & set "%~1=%R%"
exit /b 0

rem :outside_repo "path" NAME   fails (RC=1) if path is inside the Sixth Star folder
:outside_repo
set "SIXTH_A=%~1"
set "SIXTH_B=%REPO%"
powershell -NoProfile -Command "if (($env:SIXTH_A + '\').StartsWith($env:SIXTH_B + '\', 'OrdinalIgnoreCase')) { exit 1 }"
if not errorlevel 1 exit /b 0
call :say "ERROR     %~2 is inside the Sixth Star folder: %~1"
call :say "          Keep your own files, caches and data outside the repository,"
call :say "          so they can never be committed."
set "RC=1"
exit /b 1

rem :say "text"   prints a line and adds it to the log
:say
echo [run] %~1
if defined LOG >> "%LOG%" echo [run] %~1
exit /b 0

rem :tee   runs SIXTH_PROGRAM with SIXTH_ARGS, output shown and logged; RC = its exit code
:tee
powershell -NoProfile -ExecutionPolicy Bypass -File "%TEE%"
set "RC=%ERRORLEVEL%"
exit /b 0

:rebar
powershell -NoProfile -ExecutionPolicy Bypass -File "%REPO%\scripts\rebar.ps1"
set "RC=%ERRORLEVEL%"
goto :end

:help
echo.
echo  run.bat [command]
echo    (none) / run   build what changed, then run the title from your settings
echo    nobuild        run the last build without building
echo    check          build, load and bind the title, do not start it
echo    pipelines      build, then pre-build every pipeline from SHADER_ARCHIVE
echo    doctor         check the settings and dependencies only
echo    rebar          report whether Resizable BAR is active
echo    selftest       the launch layout on synthetic files (no title needed)
echo    tests          the whole test suite
echo    membench, vkbench, dxr-proto, vkrt [args]   GPU benchmarks (CSV in the log folder)
echo.
echo  Settings: run.config.bat next to run.bat (see run.config.example.bat),
echo  or set SIXTH_CONFIG to another settings file.
echo.
goto :end

:end
if defined PAUSE_AT_END (
    echo.
    if not "%RC%"=="0" echo [run] failed with exit code %RC%
    pause
)
endlocal & exit /b %RC%
