# A/B benchmark for the maintainer's own runs: starts run.bat once per
# configuration, stops each run after a fixed time, reads the runtime's own
# telemetry from each run's log and writes one comparison report.
#
#   ab.bat                                   baseline, best, bestlate, hot1024, cardbuf1
#   ab.bat -Configs baseline,hot512,hot2048 -Seconds 420 -SkipSeconds 180
#   ab.bat -ParseOnly logs\run_20261126_230000.log,logs\run_20261126_231000.log
#
# Configurations (each one run of the title):
#   baseline       the defaults
#   hot<MB>        VICTORY_HOT_WINDOWS_MB=<MB> (hot copies on the card: windows
#                  and title buffers, by use; ADR 0003)
#   cardbuf1       VICTORY_CARD_BUFFERS_MIN_MB=1, VICTORY_CARD_BUFFERS_MB=1024
#                  (buffer copies from 1 MiB on the card from the start; 4.5x
#                  less copy-back in the synthetic copyback_bench)
#   best           cardbuf1, hot1024 and texcopy together (synthetic: frame_bench
#                  109 -> 16.5 ms, late_bench 70.6 -> 24.7 ms)
#   texcopy        VICTORY_TEXTURE_FROM_COPY=1 (buffer-to-texture copies read
#                  the buffer's host copy instead of a fresh upload)
#   bestauto       best with VICTORY_HOT_WINDOWS_MB=auto (hot copies up to the
#                  card's room; they go back first when it fills)
#   bestrelay      best with VICTORY_RELAY=1 (submissions in flight, copy-back
#                  on its own thread; passes the flights tests on this machine)
#   bestlate       best with VICTORY_LATE_WAIT=3
#   wide           VICTORY_WIDE_REFRESH=1 (refreshes on several threads)
#   keepsnap       VICTORY_KEEP_SNAPSHOTS=1
#   noresidency    VICTORY_RESIDENCY=0 (the inherited texture rule)
#   nopromote      VICTORY_PROMOTE=0
#   nonote         VICTORY_BACK_NOTE=0 (copy-back pages copied again, as before bb57383)
#   relay          VICTORY_RELAY=1 (asynchronous copy-back; experimental)
#   late<N>        VICTORY_LATE_WAIT=<N> (submissions in flight; experimental)
#   name:VAR=v;VAR=v  anything else, named
#
# Your run.config.bat is never changed: each run gets a small settings file
# next to it (run.config.ab.bat, deleted afterwards) that calls yours and
# then sets the configuration's variables, the log folder and NOPAUSE.
# Nothing here reads anything but the logs the runs write.
[CmdletBinding()]
param(
    [string[]]$Configs = @('baseline', 'best', 'bestlate', 'hot1024', 'cardbuf1'),
    # How long each run lasts, from its start (loading included).
    [int]$Seconds = 300,
    # Frame lines before this many seconds after the first one are not
    # counted (loading, the menu, getting into the same scene).
    [int]$SkipSeconds = 120,
    # The settings file (default: run.config.bat next to run.bat).
    [string]$Config = '',
    # Where the logs and the report go (default: logs\ab_<time> next to the settings).
    [string]$OutDir = '',
    # Build once first (cargo; only what changed). Off: the last build.
    [switch]$Build,
    # Only read these logs (one per configuration) and write the report.
    [string[]]$ParseOnly = @(),
    # Show what would run, run nothing.
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$runBat = Join-Path $repo 'run.bat'
$bin = Join-Path $repo 'target\release\sixth-star.exe'

function Get-Overrides([string]$name) {
    switch -Regex ($name) {
        '^baseline$' { return [ordered]@{} }
        '^hot(\d+|auto)$' { return [ordered]@{ VICTORY_HOT_WINDOWS_MB = $Matches[1] } }
        '^noresidency$' { return [ordered]@{ VICTORY_RESIDENCY = '0' } }
        '^nopromote$' { return [ordered]@{ VICTORY_PROMOTE = '0' } }
        '^nonote$' { return [ordered]@{ VICTORY_BACK_NOTE = '0' } }
        '^cardbuf(\d+)$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = $Matches[1]; VICTORY_CARD_BUFFERS_MB = '1024' } }
        '^best$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^texcopy$' { return [ordered]@{ VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^bestauto$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = 'auto'; VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^bestrelay$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_RELAY = '1' } }
        '^bestlate$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_LATE_WAIT = '3' } }
        '^wide$' { return [ordered]@{ VICTORY_WIDE_REFRESH = '1' } }
        '^keepsnap$' { return [ordered]@{ VICTORY_KEEP_SNAPSHOTS = '1' } }
        '^relay$' { return [ordered]@{ VICTORY_RELAY = '1' } }
        '^late(\d)$' { return [ordered]@{ VICTORY_LATE_WAIT = $Matches[1] } }
        '^[A-Za-z0-9_-]+:(.+)$' {
            $o = [ordered]@{}
            foreach ($pair in ($Matches[1] -split ';')) {
                $kv = $pair -split '=', 2
                if ($kv.Count -ne 2 -or $kv[0] -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { throw "bad setting '$pair' in '$name'" }
                $o[$kv[0]] = $kv[1]
            }
            return $o
        }
        default { throw "unknown configuration '$name' (see the top of scripts\ab.ps1)" }
    }
}

function Get-Name([string]$name) { if ($name -match '^([A-Za-z0-9_-]+):') { $Matches[1] } else { $name } }

# ---- reading a log -----------------------------------------------------------
function Get-Median([double[]]$v) {
    if (-not $v -or $v.Count -eq 0) { return $null }
    $s = $v | Sort-Object
    $n = $s.Count
    if ($n % 2) { return [double]$s[($n - 1) / 2] }
    return ([double]$s[$n / 2 - 1] + [double]$s[$n / 2]) / 2
}

function Read-Run([string]$log) {
    $esc = [char]27
    $frames = New-Object System.Collections.Generic.List[object]
    $r = [ordered]@{
        Log = $log; FrameLines = 0; Counted = 0; Errors = 0; Removed = 0; Panics = 0; Watchdog = 0
        Incomplete = New-Object System.Collections.Generic.List[double]
        Gpu = New-Object System.Collections.Generic.List[object]
        Vram = New-Object System.Collections.Generic.List[object]
        Residency = $null; Memory = $null; Session = $null; Startup = $null; Cpu = $null; Exit = $null
        FirstError = $null
    }
    $first = $null
    $fl = '(?<fps>[\d.]+) fps, (?<ms>[\d.]+) ms/frame \((?<cpu>[\d.]+) preparing: copies (?<cms>[\d.]+) ms (?<cmb>[\d.]+) MB, textures (?<tms>[\d.]+) ms (?<tmb>[\d.]+) MB; (?<gpu>[\d.]+) on the GPU; (?<bms>[\d.]+) copying back (?<bmb>[\d.]+) MB\), (?<draws>[\d.]+) draws/frame, dropped (?<dropped>[\d.]+) without a host pipeline'
    foreach ($raw in [IO.File]::ReadLines($log)) {
        $line = $raw -replace "$esc\[[0-9;]*m", ''
        $t = $null
        if ($line -match '^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?)Z?') { $t = [datetime]::Parse($Matches[1]) }
        if ($line -match $fl) {
            $r.FrameLines++
            if (-not $first -and $t) { $first = $t }
            $frames.Add([pscustomobject]@{
                    T = $t; Fps = [double]$Matches.fps; Ms = [double]$Matches.ms; Cpu = [double]$Matches.cpu
                    CopyMs = [double]$Matches.cms; CopyMb = [double]$Matches.cmb; TexMs = [double]$Matches.tms
                    Gpu = [double]$Matches.gpu; BackMs = [double]$Matches.bms; BackMb = [double]$Matches.bmb
                    Draws = [double]$Matches.draws; Dropped = [double]$Matches.dropped
                })
            continue
        }
        if ($line -match 'INCOMPLETE FRAMES: draws and dispatches not run per frame: ([\d.]+)') { $r.Incomplete.Add([double]$Matches[1]) }
        if ($line -match 'GPU timestamps per frame \(ms\): the title''s work ([\d.]+), ours ([\d.]+) .*busy span ([\d.]+) of ([\d.]+) waited for the fence') {
            $r.Gpu.Add([pscustomobject]@{ T = $t; Title = [double]$Matches[1]; Ours = [double]$Matches[2]; Busy = [double]$Matches[3]; Wall = [double]$Matches[4] })
        }
        if ($line -match 'VRAM ([\d.]+) of ([\d.]+) GiB budget \((\d+)%\), shared GPU memory ([\d.]+) of ([\d.]+) GiB') {
            $v = [ordered]@{ T = $t; Used = [double]$Matches[1]; Budget = [double]$Matches[2]; Shared = [double]$Matches[4]; Cores = $null; Submitter = $null }
            if ($line -match 'the process kept ([\d.]+) of (\d+) logical cores busy') { $v.Cores = [double]$Matches[1] }
            if ($line -match 'the submitting thread \(\d+\) was busy (\d+)%') { $v.Submitter = [double]$Matches[1] }
            $r.Vram.Add([pscustomobject]$v)
        }
        if ($line -match '(residency: .*)$') { $r.Residency = $Matches[1] }
        if ($line -match '(host memory alive.*)$') { $r.Memory = $Matches[1] }
        if ($line -match '(session: .*)$') { $r.Session = $Matches[1] }
        if ($line -match '(startup: .*)$') { $r.Startup = $Matches[1] }
        if ($line -match '(CPU time per frame in measured work.*)$') { $r.Cpu = $Matches[1] }
        if ($line -match 'watchdog: ') { $r.Watchdog++ }
        if ($line -match '^\S*\s+ERROR |^Error: ') { $r.Errors++; if (-not $r.FirstError) { $r.FirstError = $line } }
        if ($line -match '(?i)device (was )?removed|device went|DEVICE_REMOVED|DXGI_ERROR_DEVICE') { $r.Removed++ }
        if ($line -match 'panicked at') { $r.Panics++; if (-not $r.FirstError) { $r.FirstError = $line } }
        if ($line -match '^\[run\] exit code (-?\d+)') { $r.Exit = [int64]$Matches[1] }
    }
    # The frame lines counted: from SkipSeconds after the first one (by the
    # log's clock), or the last two thirds when the lines carry no time.
    $counted = @()
    if ($first) {
        $from = $first.AddSeconds($SkipSeconds)
        $counted = @($frames | Where-Object { $_.T -and $_.T -ge $from })
    }
    if ($counted.Count -eq 0 -and $frames.Count -gt 0) {
        $counted = @($frames | Select-Object -Last ([math]::Max(1, [int]($frames.Count * 2 / 3))))
    }
    $r.Counted = $counted.Count
    # The frame time over the run, by 30 seconds from the first frame line:
    # where loading and menus end and the scene begins.
    $r.Series = ''
    if ($first) {
        $r.Series = (($frames | Where-Object { $_.T } | Group-Object { [int][math]::Floor(($_.T - $first).TotalSeconds / 30) } |
                    Sort-Object { [int]$_.Name } | ForEach-Object {
                        '{0}s {1:N0}' -f ([int]$_.Name * 30), (Get-Median (@($_.Group | ForEach-Object Ms)))
                    }) -join ', ')
    }
    $m = { param($p) Get-Median (@($counted | ForEach-Object { $_.$p })) }
    $r.Median = [ordered]@{
        Fps = & $m 'Fps'; Ms = & $m 'Ms'; Cpu = & $m 'Cpu'; CopyMs = & $m 'CopyMs'; CopyMb = & $m 'CopyMb'
        TexMs = & $m 'TexMs'; Gpu = & $m 'Gpu'; BackMs = & $m 'BackMs'; BackMb = & $m 'BackMb'
        Draws = & $m 'Draws'; Dropped = & $m 'Dropped'
    }
    $r.WorstMs = if ($counted.Count) { ($counted | Measure-Object Ms -Maximum).Maximum } else { $null }
    $gpu = @($r.Gpu | Where-Object { -not $first -or ($_.T -and $_.T -ge $first.AddSeconds($SkipSeconds)) })
    $r.GpuMedian = [ordered]@{
        Title = Get-Median (@($gpu | ForEach-Object Title)); Ours = Get-Median (@($gpu | ForEach-Object Ours))
        Busy = Get-Median (@($gpu | ForEach-Object Busy)); Wall = Get-Median (@($gpu | ForEach-Object Wall))
    }
    $vr = @($r.Vram | Where-Object { -not $first -or ($_.T -and $_.T -ge $first.AddSeconds($SkipSeconds)) })
    $r.VramMedian = [ordered]@{
        Used = Get-Median (@($vr | ForEach-Object Used)); Budget = Get-Median (@($vr | ForEach-Object Budget))
        Shared = Get-Median (@($vr | ForEach-Object Shared))
        Cores = Get-Median (@($vr | Where-Object { $null -ne $_.Cores } | ForEach-Object Cores))
        Submitter = Get-Median (@($vr | Where-Object { $null -ne $_.Submitter } | ForEach-Object Submitter))
    }
    $r.VramPeak = if ($r.Vram.Count) { ($r.Vram | Measure-Object Used -Maximum).Maximum } else { $null }
    $r.IncompleteLast = if ($r.Incomplete.Count) { $r.Incomplete[$r.Incomplete.Count - 1] } else { 0 }
    return [pscustomobject]$r
}

function F($v, [string]$fmt = '{0:N1}') { if ($null -eq $v) { 'n/a' } else { $fmt -f $v } }

function Write-Report($runs, [string]$path, $info) {
    $out = New-Object System.Collections.Generic.List[string]
    $out.Add('# Sixth Star A/B report')
    $out.Add('')
    $out.Add("Written $(Get-Date -Format 'yyyy-MM-dd HH:mm'). Each figure is the median of the frame lines counted (from $SkipSeconds s after the first frame line). Approximate; one machine, one run each.")
    $out.Add('')
    foreach ($k in $info.Keys) { $out.Add("- ${k}: $($info[$k])") }
    $out.Add('')
    $head = '| Measure | ' + (($runs | ForEach-Object { $_.Name }) -join ' | ') + ' |'
    $out.Add($head)
    $out.Add('|---|' + (($runs | ForEach-Object { '---' }) -join '|') + '|')
    $rows = @(
        @('Settings', { param($x) if ($x.Overrides.Count) { ($x.Overrides.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ' } else { 'defaults' } }),
        @('Frame lines counted (of all)', { param($x) "$($x.Run.Counted) of $($x.Run.FrameLines)" }),
        @('FPS', { param($x) F $x.Run.Median.Fps '{0:N2}' }),
        @('ms/frame', { param($x) F $x.Run.Median.Ms }),
        @('  against the first configuration', { param($x)
                $base = $runs[0].Run.Median.Ms
                $ms = $x.Run.Median.Ms
                if ($null -eq $base -or $null -eq $ms -or $base -eq 0) { 'n/a' }
                elseif ($x -eq $runs[0]) { '(the reference)' }
                else { '{0:+0;-0}% ({1:N2}x the FPS)' -f (($ms - $base) / $base * 100), ($base / $ms) }
            }),
        @('worst frame line, ms/frame', { param($x) F $x.Run.WorstMs }),
        @('CPU preparing, ms', { param($x) F $x.Run.Median.Cpu }),
        @('  of it copies, ms (MB)', { param($x) "$(F $x.Run.Median.CopyMs) ($(F $x.Run.Median.CopyMb))" }),
        @('  of it textures, ms', { param($x) F $x.Run.Median.TexMs }),
        @('on the GPU (fence wait), ms', { param($x) F $x.Run.Median.Gpu }),
        @('copying back, ms (MB)', { param($x) "$(F $x.Run.Median.BackMs) ($(F $x.Run.Median.BackMb))" }),
        @('GPU timestamps: title / ours / busy of wall, ms', { param($x) "$(F $x.Run.GpuMedian.Title) / $(F $x.Run.GpuMedian.Ours) / $(F $x.Run.GpuMedian.Busy) of $(F $x.Run.GpuMedian.Wall)" }),
        @('draws/frame', { param($x) F $x.Run.Median.Draws '{0:N0}' }),
        @('dropped without a pipeline/frame', { param($x) F $x.Run.Median.Dropped }),
        @('INCOMPLETE FRAMES (lines; last per frame)', { param($x) "$($x.Run.Incomplete.Count); $(F $x.Run.IncompleteLast)" }),
        @('VRAM used of budget, GiB (peak)', { param($x) "$(F $x.Run.VramMedian.Used '{0:N2}') of $(F $x.Run.VramMedian.Budget '{0:N2}') ($(F $x.Run.VramPeak '{0:N2}'))" }),
        @('shared GPU memory, GiB', { param($x) F $x.Run.VramMedian.Shared '{0:N2}' }),
        @('cores busy (process)', { param($x) F $x.Run.VramMedian.Cores '{0:N2}' }),
        @('submitting thread busy, %', { param($x) F $x.Run.VramMedian.Submitter '{0:N0}' }),
        @('watchdog lines (stalls)', { param($x) "$($x.Run.Watchdog)" }),
        @('errors / device removed / panics', { param($x) "$($x.Run.Errors) / $($x.Run.Removed) / $($x.Run.Panics)" }),
        @('ran, s (exit code)', { param($x) "$(F $x.Seconds '{0:N0}') ($(if ($null -ne $x.Run.Exit) { $x.Run.Exit } else { 'n/a' }))" })
    )
    foreach ($row in $rows) {
        $cells = $runs | ForEach-Object { (& $row[1] $_) -replace '\|', '/' }
        $out.Add("| $($row[0]) | " + ($cells -join ' | ') + ' |')
    }
    foreach ($x in $runs) {
        $out.Add('')
        $out.Add("## $($x.Name)")
        $out.Add('')
        $out.Add("- log: $($x.Run.Log)")
        if ($x.Run.Series) { $out.Add("- ms/frame by 30 s from the first frame line: $($x.Run.Series)") }
        foreach ($p in @('Session', 'Startup', 'Residency', 'Memory', 'Cpu', 'FirstError')) {
            if ($x.Run.$p) { $out.Add("- last ${p}: ``$($x.Run.$p)``") }
        }
    }
    $out.Add('')
    $out.Add('Send this file back as it is. It holds no paths but the logs'' and no game data.')
    [IO.File]::WriteAllLines($path, $out, (New-Object Text.UTF8Encoding($false)))
    $out | ForEach-Object { Write-Host $_ }
}

function Get-Build {
    $b = [ordered]@{}
    try {
        $b['commit'] = (& git -C $repo rev-parse --short HEAD 2>$null)
        $dirty = (& git -C $repo status --porcelain --untracked-files=no 2>$null)
        $b['working tree'] = if ($dirty) { 'changed files (not committed)' } else { 'clean' }
    } catch { $b['commit'] = 'unknown (no git)' }
    if (Test-Path $bin) {
        $f = Get-Item $bin
        $hash = (Get-FileHash $bin -Algorithm SHA256).Hash.Substring(0, 16)
        $b['binary'] = "sixth-star.exe built $($f.LastWriteTime.ToString('yyyy-MM-dd HH:mm')), sha256 $hash"
    }
    $b['machine'] = "$((Get-CimInstance Win32_VideoController | Select-Object -First 1).Name); $((Get-CimInstance Win32_Processor | Select-Object -First 1).Name)"
    return $b
}

# ---- parse only ------------------------------------------------------------------
if ($ParseOnly.Count) {
    $logs = @($ParseOnly | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
    $names = @($Configs | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
    $runs = for ($i = 0; $i -lt $logs.Count; $i++) {
        $name = if ($i -lt $names.Count) { $names[$i] } else { "run$($i + 1)" }
        [pscustomobject]@{ Name = (Get-Name $name); Overrides = (Get-Overrides $name); Run = (Read-Run (Resolve-Path $logs[$i]).Path); Seconds = $null }
    }
    if (-not $OutDir) { $OutDir = Split-Path -Parent (Resolve-Path $logs[0]).Path }
    Write-Report $runs (Join-Path $OutDir 'ab-report.md') (Get-Build)
    exit 0
}

# ---- runs ------------------------------------------------------------------------
$Configs = @($Configs | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
foreach ($c in $Configs) { $null = Get-Overrides $c }   # bad names fail before anything runs
if (-not $Config) { $Config = Join-Path $repo 'run.config.bat' }
if (-not (Test-Path $Config)) { throw "no settings file: $Config (run run.bat once first)" }
$Config = (Resolve-Path $Config).Path
$configDir = Split-Path -Parent $Config
if (-not $OutDir) { $OutDir = Join-Path $configDir ("logs\ab_" + (Get-Date -Format 'yyyyMMdd_HHmmss')) }
$null = New-Item -ItemType Directory -Force $OutDir
$OutDir = (Resolve-Path $OutDir).Path
$wrapper = Join-Path $configDir 'run.config.ab.bat'
Write-Host "[ab] settings  $Config (not changed)"
Write-Host "[ab] logs      $OutDir"
Write-Host "[ab] runs      $($Configs -join ', '), $Seconds s each, counting from $SkipSeconds s after the first frame line"
Write-Host '[ab] each run: get into the same scene and stay there; the run stops by itself.'
if ($DryRun) {
    foreach ($c in $Configs) { Write-Host "[ab] would run $(Get-Name $c): $((Get-Overrides $c).GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" })" }
    exit 0
}
if ($Build) {
    Write-Host '[ab] building (release; only what changed)'
    & cargo build --release -p victory --manifest-path (Join-Path $repo 'Cargo.toml')
    if ($LASTEXITCODE) { throw 'the build failed' }
}
if (-not (Test-Path $bin)) { throw "no build yet: $bin (use -Build)" }
$buildInfo = Get-Build
$runs = New-Object System.Collections.Generic.List[object]
try {
    foreach ($c in $Configs) {
        $name = Get-Name $c
        $over = Get-Overrides $c
        $dir = Join-Path $OutDir $name
        $null = New-Item -ItemType Directory -Force $dir
        $lines = @('@echo off', "rem Written by scripts\ab.ps1 for one run; deleted afterwards.", "call `"$Config`"", "set `"LOG_DIR=$dir`"")
        # Every switch the configurations use is cleared first, so a
        # setting in run.config.bat does not leak into another configuration.
        foreach ($v in @('VICTORY_HOT_WINDOWS_MB', 'VICTORY_RESIDENCY', 'VICTORY_PROMOTE', 'VICTORY_RELAY', 'VICTORY_LATE_WAIT',
                'VICTORY_CARD_BUFFERS_MIN_MB', 'VICTORY_CARD_BUFFERS_MB', 'VICTORY_WIDE_REFRESH', 'VICTORY_KEEP_SNAPSHOTS', 'VICTORY_BACK_NOTE',
                'VICTORY_TEXTURE_FROM_COPY')) {
            if (-not $over.Contains($v)) { $lines += "set `"$v=`"" }
        }
        foreach ($k in $over.Keys) { $lines += "set `"$k=$($over[$k])`"" }
        [IO.File]::WriteAllLines($wrapper, $lines, (New-Object Text.ASCIIEncoding))
        Write-Host ''
        Write-Host "[ab] ---- $name ($(if ($over.Count) { ($over.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ' } else { 'defaults' })) ----"
        $env:SIXTH_CONFIG = $wrapper
        $env:NOPAUSE = '1'
        $started = Get-Date
        $p = Start-Process -FilePath $env:ComSpec -ArgumentList '/c', "`"$runBat`" nobuild" -PassThru -NoNewWindow
        $deadline = $started.AddSeconds($Seconds)
        while (-not $p.HasExited -and (Get-Date) -lt $deadline) {
            Start-Sleep -Seconds 2
            $left = [int]($deadline - (Get-Date)).TotalSeconds
            if ($left % 30 -lt 2) { Write-Host "[ab] $name`: $left s left" }
        }
        $ran = ((Get-Date) - $started).TotalSeconds
        if (-not $p.HasExited) {
            Write-Host "[ab] $name`: time is up, stopping the run"
            Get-Process sixth-star -ErrorAction SilentlyContinue | Where-Object { $_.StartTime -ge $started } | Stop-Process -Force
            $null = $p.WaitForExit(30000)
        } else {
            Write-Host "[ab] $name`: the run ended by itself after $([int]$ran) s (exit code $($p.ExitCode))"
        }
        Start-Sleep -Seconds 3   # let the device and the log close
        $log = Get-ChildItem $dir -Filter '*.log' | Sort-Object LastWriteTime | Select-Object -Last 1
        if (-not $log) { Write-Host "[ab] $name`: no log written"; continue }
        $runs.Add([pscustomobject]@{ Name = $name; Overrides = $over; Run = (Read-Run $log.FullName); Seconds = $ran })
    }
} finally {
    Remove-Item $wrapper -ErrorAction SilentlyContinue
    Remove-Item Env:SIXTH_CONFIG -ErrorAction SilentlyContinue
}
if ($runs.Count) {
    $report = Join-Path $OutDir 'ab-report.md'
    Write-Report $runs $report $buildInfo
    Write-Host ''
    Write-Host "[ab] report: $report"
}
