@echo off
rem ===========================================================================
rem  Sixth Star launcher settings. run.bat copies this file to run.config.bat
rem  the first time; fill in run.config.bat (it is never committed).
rem
rem  Paths may be absolute (C:\..., \\server\...) or relative:
rem    GAME_DIR, FFX_DIR, SKIP_LIST, LOG_DIR, CACHE_DIR, DATA_DIR
rem                                               relative to this file's folder
rem    TITLE_EXE, WORK_DIR, MANIFEST, SHADER_ARCHIVE   relative to GAME_DIR
rem ===========================================================================

rem The package folder of a title you dumped from your own console (the folder
rem with MicrosoftGame.config). Keep it OUTSIDE this repository.
set "GAME_DIR="

rem The title's executable, inside GAME_DIR or a subfolder of it.
set "TITLE_EXE="

rem Optional: the title's working directory (default: GAME_DIR) and a
rem manifest to use instead of GAME_DIR\MicrosoftGame.config.
set "WORK_DIR="
set "MANIFEST="

rem series-x (default) or series-s (720p + FSR; needs FFX_DIR).
set "CONSOLE=series-x"

rem Only for series-s: the FidelityFX SDK's ...\Kits\FidelityFX\signedbin.
set "FFX_DIR="

rem safe (default) or fast (lower latency; can be less stable on some GPUs).
set "PRESET=safe"

rem 0 = translate shaders as they are, 1 = optimise them (experimental).
set "SHADER_OPT=0"

rem Shaders whose compile never finishes on your GPU driver, by hash:
rem comma-separated in SKIP_PIPELINES, or one file with the list in SKIP_LIST.
rem Leave both empty unless you know a shader hangs your driver.
set "SKIP_PIPELINES="
set "SKIP_LIST="

rem For "run.bat pipelines": the title's shader archive, to pre-build every
rem pipeline once.
set "SHADER_ARCHIVE="

rem 1 = D3D12 DRED (more detail when the GPU device is removed; a bit slower).
set "DRED=0"

rem Where logs go (default: logs\ next to this file).
set "LOG_DIR="

rem Where translated shaders and other caches go, and the title's own data
rem (its saves and temporary files). Default: %LOCALAPPDATA%\victory\cache and
rem ...\data, on C:. Set these to put them on another drive, for example
rem   set "CACHE_DIR=D:\SixthStar\cache"
rem   set "DATA_DIR=D:\SixthStar\data"
rem Moving DATA_DIR later: move the old data folder too, or the title's saves
rem stay behind in the old one. Neither may be inside the Sixth Star folder.
set "CACHE_DIR="
set "DATA_DIR="
