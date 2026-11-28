# A/B benchmark for the maintainer's own runs: starts run.bat once per
# configuration, stops each run after a fixed time, reads the runtime's own
# telemetry from each run's log and writes one comparison report.
#
#   ab.bat                                   baseline, resid, best, hot1024, cardbuf1
#   ab.bat -Configs baseline,hot512,hot2048 -Seconds 420 -SkipSeconds 180
#   ab.bat -ParseOnly logs\run_20261126_230000.log,logs\run_20261126_231000.log
#   ab.bat -Stability                        one run of 33 minutes (baseline, or the
#                                            -Configs given): the session length the
#                                            leaderboard's stability lane asks for
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
#   texplain       texcopy with VICTORY_PLAIN_COPY=1 (host copies of title memory
#                  read in place, not through the kernel; stand-in title,
#                  2026-11-27: CPU preparing 2.7 -> 1.7 ms at 8 copies)
#   bestplain      best with VICTORY_PLAIN_COPY=1
#   watched        VICTORY_WATCHED_ALLOC=1 (large blocks of the default allocator,
#                  so committed buffers unless the title hooks it, are
#                  write-watched; stand-in title, 2026-11-28: 10% of each buffer
#                  rewritten 2.5 -> 0.3 ms CPU preparing, whole buffers 2.6 -> 3.2)
#   texwatched     texcopy with VICTORY_WATCHED_ALLOC=1
#   texhot         texwatched with VICTORY_HOT_SKIP=1 (buffers found wholly written
#                  twice in a row skip the write-watch asking for 8 askings)
#   seriess        CONSOLE=series-s with VICTORY_FFX_SIMPLE=1 (the Series S
#                  profile, 720p, with a bilinear upscale: no FidelityFX SDK needed)
#   seriessbest    seriess with the best switches
#   wide          VICTORY_WIDE_REFRESH=1 (refreshes on several threads)
#   fills          VICTORY_WIDE_FILLS=1 (a list's fills on several threads; the default
#                  since 2026-11-27)
#   nofills        VICTORY_WIDE_FILLS=0 (fills one after the other, as before)
#   keepsnap       VICTORY_KEEP_SNAPSHOTS=1
#   resid          VICTORY_RESIDENCY=1 (textures that find no room on the card go
#                  to the PC's memory; the default from 009dd7a until 2026-11-27,
#                  when the first title run with it was several times slower)
#   noresidency    VICTORY_RESIDENCY=0 (the inherited texture rule; the default
#                  again since 2026-11-27)
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
    [string[]]$Configs = @('baseline', 'resid', 'best', 'hot1024', 'cardbuf1'),
    # How long each run lasts, from its start (loading included).
    [int]$Seconds = 300,
    # Frame lines before this many seconds after the first one are not
    # counted (loading, the menu, getting into the same scene).
    [int]$SkipSeconds = 120,
    # Only frame lines with at least this many draws per frame count as
    # gameplay: menus and loading screens draw a few dozen (13 in the
    # 2026-11-27 runs at 30 FPS), gameplay well over a thousand. The other
    # lines of the runtime (GPU, VRAM) count with the frame line nearest them.
    [int]$MinDraws = 200,
    # The settings file (default: run.config.bat next to run.bat).
    [string]$Config = '',
    # Where the logs and the report go (default: logs\ab_<time> next to the settings).
    [string]$OutDir = '',
    # Build once first (cargo; only what changed). Off: the last build.
    [switch]$Build,
    # Only read these logs (one per configuration) and write the report.
    [string[]]$ParseOnly = @(),
    # Show what would run, run nothing.
    [switch]$DryRun,
    # One long run for the stability record: 33 minutes, so that the
    # runner's session line passes 30 minutes after the first frame.
    [switch]$Stability,
    # Also write the report as ab-report.json (for scripts/autotest.ps1).
    [switch]$Json,
    # Print the switches of this configuration as JSON and stop (for
    # scripts/autotest.ps1, which runs the same configurations).
    [string]$Overrides = ''
)
if ($Stability) {
    if (-not $PSBoundParameters.ContainsKey('Configs')) { $Configs = @('baseline') }
    if (-not $PSBoundParameters.ContainsKey('Seconds')) { $Seconds = 33 * 60 }
}
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$runBat = Join-Path $repo 'run.bat'
$bin = Join-Path $repo 'target\release\sixth-star.exe'

function Get-Overrides([string]$name) {
    switch -Regex ($name) {
        '^baseline$' { return [ordered]@{} }
        '^hot(\d+|auto)$' { return [ordered]@{ VICTORY_HOT_WINDOWS_MB = $Matches[1] } }
        '^resid$' { return [ordered]@{ VICTORY_RESIDENCY = '1' } }
        '^noresidency$' { return [ordered]@{ VICTORY_RESIDENCY = '0' } }
        '^nopromote$' { return [ordered]@{ VICTORY_PROMOTE = '0' } }
        '^nonote$' { return [ordered]@{ VICTORY_BACK_NOTE = '0' } }
        '^cardbuf(\d+)$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = $Matches[1]; VICTORY_CARD_BUFFERS_MB = '1024' } }
        '^best$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^texcopy$' { return [ordered]@{ VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^bestauto$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = 'auto'; VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^bestrelay$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_RELAY = '1' } }
        '^bestlate$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_LATE_WAIT = '3' } }
        '^texplain$' { return [ordered]@{ VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_PLAIN_COPY = '1' } }
        '^bestplain$' { return [ordered]@{ VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_PLAIN_COPY = '1' } }
        '^watched$' { return [ordered]@{ VICTORY_WATCHED_ALLOC = '1' } }
        '^texwatched$' { return [ordered]@{ VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_WATCHED_ALLOC = '1' } }
        '^texhot$' { return [ordered]@{ VICTORY_TEXTURE_FROM_COPY = '1'; VICTORY_WATCHED_ALLOC = '1'; VICTORY_HOT_SKIP = '1' } }
        '^seriess$' { return [ordered]@{ CONSOLE = 'series-s'; VICTORY_FFX_SIMPLE = '1' } }
        '^seriessbest$' { return [ordered]@{ CONSOLE = 'series-s'; VICTORY_FFX_SIMPLE = '1'; VICTORY_CARD_BUFFERS_MIN_MB = '1'; VICTORY_CARD_BUFFERS_MB = '1024'; VICTORY_HOT_WINDOWS_MB = '1024'; VICTORY_TEXTURE_FROM_COPY = '1' } }
        '^wide$' { return [ordered]@{ VICTORY_WIDE_REFRESH = '1' } }
        '^fills$' { return [ordered]@{ VICTORY_WIDE_FILLS = '1' } }
        '^nofills$' { return [ordered]@{ VICTORY_WIDE_FILLS = '0' } }
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

if ($Overrides) {
    $o = Get-Overrides $Overrides
    $h = @{}
    foreach ($k in $o.Keys) { $h[$k] = $o[$k] }
    $h | ConvertTo-Json -Compress
    exit 0
}

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
        SessionS = $null; SessionFrames = $null; SessionFps = $null; SessionMs = $null; WorstFps = $null; LongestGap = $null; NotRun = $null
        LowestRamFree = $null; MenuLines = 0; Commit = $null
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
            $v = [ordered]@{ T = $t; Used = [double]$Matches[1]; Budget = [double]$Matches[2]; Shared = [double]$Matches[4]; Cores = $null; Submitter = $null
                RamFree = $null; RamTotal = $null; Commit = $null; CommitLimit = $null; Private = $null; WorkingSet = $null; Faults = $null }
            if ($line -match 'the process kept ([\d.]+) of (\d+) logical cores busy') { $v.Cores = [double]$Matches[1] }
            if ($line -match 'the submitting thread \(\d+\) was busy (\d+)%') { $v.Submitter = [double]$Matches[1] }
            if ($line -match 'the PC''s memory: ([\d.]+) of ([\d.]+) GiB in use, ([\d.]+) GiB free; the system''s commit ([\d.]+) of ([\d.]+) GiB;\s+this process committed ([\d.]+) GiB, ([\d.]+) GiB of it in RAM') {
                $v.RamTotal = [double]$Matches[2]; $v.RamFree = [double]$Matches[3]; $v.Commit = [double]$Matches[4]; $v.CommitLimit = [double]$Matches[5]
                $v.Private = [double]$Matches[6]; $v.WorkingSet = [double]$Matches[7]
            }
            if ($line -match '([\d.]+) page faults/s') { $v.Faults = [double]$Matches[1] }
            $r.Vram.Add([pscustomobject]$v)
        }
        if ($line -match '(residency: .*)$') { $r.Residency = $Matches[1] }
        if ($line -match '(host memory alive.*)$') { $r.Memory = $Matches[1] }
        if ($line -match '(session: .*)$') {
            $r.Session = $Matches[1]
            if ($r.Session -match 'session: (\d+) min (\d+) s since the first present, (\d+) frames, ([\d.]+) FPS average \(([\d.]+) ms/frame\), worst second ([\d.]+) FPS, longest time between two presents ([\d.]+) s; (\d+) of (\d+) seconds with draws not run') {
                $r.SessionS = [int]$Matches[1] * 60 + [int]$Matches[2]
                $r.SessionFrames = [int64]$Matches[3]
                $r.SessionFps = [double]$Matches[4]
                $r.SessionMs = [double]$Matches[5]
                $r.WorstFps = [double]$Matches[6]
                $r.LongestGap = [double]$Matches[7]
                $r.NotRun = "$($Matches[8]) of $($Matches[9])"
            }
            if ($r.Session -match 'the PC''s free memory at its lowest ([\d.]+) GiB') { $r.LowestRamFree = [double]$Matches[1] }
        }
        if ($line -match '(startup: .*)$') { $r.Startup = $Matches[1] }
        if ($line -match '(CPU time per frame in measured work.*)$') { $r.Cpu = $Matches[1] }
        if ($line -match 'watchdog: ') { $r.Watchdog++ }
        if ($line -match '^\S*\s+ERROR |^Error: ') { $r.Errors++; if (-not $r.FirstError) { $r.FirstError = $line } }
        if ($line -match '(?i)device (was )?removed|device went|DEVICE_REMOVED|DXGI_ERROR_DEVICE') { $r.Removed++ }
        if ($line -match 'panicked at') { $r.Panics++; if (-not $r.FirstError) { $r.FirstError = $line } }
        if ($line -match '^\[run\] exit code (-?\d+)') { $r.Exit = [int64]$Matches[1] }
        if ($line -match '^\[run\] commit\s+(\S+)') { $r.Commit = $Matches[1] }
    }
    # The frame lines counted: from SkipSeconds after the first one (by the
    # log's clock), or the last two thirds when the lines carry no time.
    $counted = @()
    $late = $frames.ToArray()
    if ($first) {
        $from = $first.AddSeconds($SkipSeconds)
        $late = @($frames | Where-Object { $_.T -and $_.T -ge $from })
    }
    if ($late.Count -eq 0 -and $frames.Count -gt 0 -and -not $first) {
        $late = @($frames | Select-Object -Last ([math]::Max(1, [int]($frames.Count * 2 / 3))))
    }
    # Gameplay only: the frame lines that draw like a scene, not a menu.
    $counted = @($late | Where-Object { $_.Draws -ge $MinDraws })
    $r.MenuLines = $late.Count - $counted.Count
    $r.Counted = $counted.Count
    # Another line of the runtime counts when the frame line nearest it in
    # time is counted (they are written in the same batches).
    $timed = @($frames.ToArray() | Where-Object { $_.T })
    $inGameplay = {
        param($t)
        if (-not $t -or $timed.Count -eq 0) { return $true }
        $best = $null; $gap = [double]::MaxValue
        foreach ($f in $timed) {
            $d = [math]::Abs(($f.T - $t).TotalSeconds)
            if ($d -lt $gap) { $gap = $d; $best = $f }
        }
        return ($counted -contains $best)
    }
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
    $gpu = @($r.Gpu | Where-Object { (-not $first -or ($_.T -and $_.T -ge $first.AddSeconds($SkipSeconds))) -and (& $inGameplay $_.T) })
    $r.GpuMedian = [ordered]@{
        Title = Get-Median (@($gpu | ForEach-Object Title)); Ours = Get-Median (@($gpu | ForEach-Object Ours))
        Busy = Get-Median (@($gpu | ForEach-Object Busy)); Wall = Get-Median (@($gpu | ForEach-Object Wall))
    }
    $vr = @($r.Vram | Where-Object { (-not $first -or ($_.T -and $_.T -ge $first.AddSeconds($SkipSeconds))) -and (& $inGameplay $_.T) })
    $med = { param($p) Get-Median (@($vr | Where-Object { $null -ne $_.$p } | ForEach-Object { $_.$p })) }
    $r.VramMedian = [ordered]@{
        Used = & $med 'Used'; Budget = & $med 'Budget'; Shared = & $med 'Shared'
        Cores = & $med 'Cores'; Submitter = & $med 'Submitter'
        RamFree = & $med 'RamFree'; RamTotal = & $med 'RamTotal'; Commit = & $med 'Commit'; CommitLimit = & $med 'CommitLimit'
        Private = & $med 'Private'; WorkingSet = & $med 'WorkingSet'; Faults = & $med 'Faults'
    }
    $free = @($r.Vram | Where-Object { $null -ne $_.RamFree } | ForEach-Object RamFree)
    $r.RamFreeLowest = if ($free.Count) { ($free | Measure-Object -Minimum).Minimum } else { $r.LowestRamFree }
    $r.VramPeak = if ($r.Vram.Count) { ($r.Vram | Measure-Object Used -Maximum).Maximum } else { $null }
    $r.IncompleteLast = if ($r.Incomplete.Count) { $r.Incomplete[$r.Incomplete.Count - 1] } else { 0 }
    return [pscustomobject]$r
}

function F($v, [string]$fmt = '{0:N1}') { if ($null -eq $v) { 'n/a' } else { $fmt -f $v } }

function Write-Report($runs, [string]$path, $info) {
    $out = New-Object System.Collections.Generic.List[string]
    $out.Add('# Sixth Star A/B report')
    $out.Add('')
    $out.Add("Written $(Get-Date -Format 'yyyy-MM-dd HH:mm'). Each figure is the median of the gameplay frame lines: from $SkipSeconds s after the first frame line, and only lines with at least $MinDraws draws per frame (menus and loading screens draw far fewer and are left out). The runtime's other lines count with the frame line nearest them. Approximate; one machine, one run each.")
    $out.Add('')
    foreach ($k in $info.Keys) { $out.Add("- ${k}: $($info[$k])") }
    $out.Add('')
    $head = '| Measure | ' + (($runs | ForEach-Object { $_.Name }) -join ' | ') + ' |'
    $out.Add($head)
    $out.Add('|---|' + (($runs | ForEach-Object { '---' }) -join '|') + '|')
    $rows = @(
        @('Commit the run was built from', { param($x) if ($x.Run.Commit) { $x.Run.Commit } else { 'not in the log (runs before e847242)' } }),
        @('Settings', { param($x) if ($x.Overrides.Count) { ($x.Overrides.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ' } else { 'defaults' } }),
        @('Gameplay frame lines counted (of all; menu or loading lines left out)', { param($x) "$($x.Run.Counted) of $($x.Run.FrameLines) ($($x.Run.MenuLines) left out)" }),
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
        @('the PC''s memory free, GiB (lowest of the run)', { param($x) "$(F $x.Run.VramMedian.RamFree) ($(F $x.Run.RamFreeLowest)) of $(F $x.Run.VramMedian.RamTotal)" }),
        @('the system''s commit of its limit, GiB', { param($x) "$(F $x.Run.VramMedian.Commit) of $(F $x.Run.VramMedian.CommitLimit)" }),
        @('the process: committed / in RAM, GiB', { param($x) "$(F $x.Run.VramMedian.Private '{0:N2}') / $(F $x.Run.VramMedian.WorkingSet '{0:N2}')" }),
        @('page faults/s (process)', { param($x) F $x.Run.VramMedian.Faults '{0:N0}' }),
        @('cores busy (process)', { param($x) F $x.Run.VramMedian.Cores '{0:N2}' }),
        @('submitting thread busy, %', { param($x) F $x.Run.VramMedian.Submitter '{0:N0}' }),
        @('watchdog lines (stalls)', { param($x) "$($x.Run.Watchdog)" }),
        @('errors / device removed / panics', { param($x) "$($x.Run.Errors) / $($x.Run.Removed) / $($x.Run.Panics)" }),
        @('ran, s (exit code)', { param($x) "$(F $x.Seconds '{0:N0}') ($(if ($null -ne $x.Run.Exit) { $x.Run.Exit } else { 'n/a' }))" }),
        @('how it ended', { param($x) if ($x.Stopped) { 'ran the full time, stopped by the script: no crash' } elseif ($null -eq $x.Stopped) { 'n/a (log read only)' } else { 'ended by itself before the time was up: a crash or an exit' } }),
        @('session line: length since the first frame', { param($x) if ($null -ne $x.Run.SessionS) { '{0} min {1} s' -f [int][math]::Floor($x.Run.SessionS / 60), ($x.Run.SessionS % 60) } else { 'no session line' } }),
        @('session line: FPS average (ms/frame), menus included', { param($x) if ($null -ne $x.Run.SessionFps) { '{0:N2} ({1:N1})' -f $x.Run.SessionFps, $x.Run.SessionMs } else { 'n/a' } }),
        @('session line: worst second, longest freeze', { param($x) if ($null -ne $x.Run.WorstFps) { '{0:N2} FPS, {1:N2} s' -f $x.Run.WorstFps, $x.Run.LongestGap } else { 'n/a' } }),
        @('session line: seconds with draws not run', { param($x) if ($x.Run.NotRun) { $x.Run.NotRun } else { 'n/a' } })
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
    # What STATUS.md can say, from the session lines only (measured, not
    # estimated): one line per run that wrote one.
    $out.Add('')
    $out.Add('## For STATUS.md (gameplay from the frame lines; session length from the session line)')
    $out.Add('')
    foreach ($x in $runs) {
        $settings = if ($x.Overrides.Count) { ($x.Overrides.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ', ' } else { 'default settings' }
        $game = if ($x.Run.Counted -gt 0) {
            "gameplay $('{0:N2}' -f $x.Run.Median.Fps) FPS ($('{0:N1}' -f $x.Run.Median.Ms) ms/frame), the median of $($x.Run.Counted) frame lines with at least $MinDraws draws per frame"
        } else {
            "no gameplay measured (no frame line with at least $MinDraws draws per frame after $SkipSeconds s: menus or loading only)"
        }
        if ($null -eq $x.Run.SessionS) {
            $out.Add("- $($x.Name) ($settings): $game; no session line in the log (the run ended before a minute of frames).")
            continue
        }
        $len = '{0} min {1} s' -f [int][math]::Floor($x.Run.SessionS / 60), ($x.Run.SessionS % 60)
        $ended = if ($x.Stopped) { 'without a crash (the run was stopped by the A/B script when its time was up)' } elseif ($null -eq $x.Stopped) { '(how the run ended is not known from the log alone)' } else { 'and then the run ended by itself (crash, exit or closed by hand)' }
        $build = if ($x.Run.Commit) { $x.Run.Commit } elseif ($null -eq $x.Stopped) { 'not recorded in the log' } else { $info['commit'] }
        $out.Add("- $(Get-Date -Format 'yyyy-MM-dd'), build $build, $settings`: $game; a session of at least $len after the first frame $ended, worst second $('{0:N2}' -f $x.Run.WorstFps) FPS, longest time between two presents $('{0:N2}' -f $x.Run.LongestGap) s. The session line's average ($('{0:N2}' -f $x.Run.SessionFps) FPS) counts the menus too and is not a gameplay figure. Measured by the maintainer from the runner's own lines.")
    }
    $out.Add('')
    $out.Add('Send this file back as it is. It holds no paths but the logs'' and no game data.')
    [IO.File]::WriteAllLines($path, $out, (New-Object Text.UTF8Encoding($false)))
    if ($Json) {
        $json = foreach ($x in $runs) {
            $r = $x.Run
            [ordered]@{
                name = $x.Name; settings = $x.Overrides; commit = $r.Commit; log = $r.Log
                frameLines = $r.FrameLines; counted = $r.Counted; menuLines = $r.MenuLines
                median = $r.Median; worstMs = $r.WorstMs; gpu = $r.GpuMedian; vram = $r.VramMedian
                vramPeak = $r.VramPeak; ramFreeLowest = $r.RamFreeLowest
                errors = $r.Errors; removed = $r.Removed; panics = $r.Panics; watchdog = $r.Watchdog
                incompleteLines = $r.Incomplete.Count; incompleteLast = $r.IncompleteLast
                sessionS = $r.SessionS; sessionFps = $r.SessionFps; worstFps = $r.WorstFps; longestGap = $r.LongestGap
                firstError = $r.FirstError; exit = $r.Exit; seconds = $x.Seconds; stopped = $x.Stopped
            }
        }
        $jsonPath = [IO.Path]::ChangeExtension($path, '.json')
        [IO.File]::WriteAllText($jsonPath, (ConvertTo-Json -InputObject @($json) -Depth 6), (New-Object Text.UTF8Encoding($false)))
    }
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
        [pscustomobject]@{ Name = (Get-Name $name); Overrides = (Get-Overrides $name); Run = (Read-Run (Resolve-Path $logs[$i]).Path); Seconds = $null; Stopped = $null }
    }
    if (-not $OutDir) { $OutDir = Split-Path -Parent (Resolve-Path $logs[0]).Path }
    $now = Get-Build
    $now['commit'] = "$($now['commit']) now (logs read afterwards: each run's own commit is in the table when its log names it)"
    Write-Report $runs (Join-Path $OutDir 'ab-report.md') $now
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
# The maintainer's own launcher beside the settings file, if there is one:
# it sets local defaults before it calls this repository's run.bat (the
# skip list of shaders that never finish compiling on that driver, the
# caches on another drive). Run without it, every configuration started
# differently from the maintainer's own runs.
$localRun = Join-Path $configDir 'run.bat'
if ((Test-Path $localRun) -and ((Resolve-Path $localRun).Path -ne (Resolve-Path $runBat).Path)) {
    $runBat = (Resolve-Path $localRun).Path
}
Write-Host "[ab] launcher  $runBat"
foreach ($pair in @(@('CACHE_DIR', 'victory-cache'), @('DATA_DIR', 'victory-data'))) {
    $folder = Join-Path $configDir $pair[1]
    if (Test-Path $folder) { Write-Host "[ab] $($pair[0].ToLower().PadRight(9)) $folder (unless the settings name another)" }
}
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
        # The caches and the title's data where the maintainer's own runs
        # keep them: a victory-cache / victory-data folder next to the
        # settings file, unless the settings name others. Without them the
        # runs fell back to the default on C:, an empty shader cache: every
        # shader translated and every pipeline built again, draws skipped
        # meanwhile (frames of seconds, a black picture), in every config.
        foreach ($pair in @(@('CACHE_DIR', 'victory-cache'), @('DATA_DIR', 'victory-data'))) {
            $folder = Join-Path $configDir $pair[1]
            if (Test-Path $folder) { $lines += "if not defined $($pair[0]) set `"$($pair[0])=$folder`"" }
        }
        # Every switch the configurations use is cleared first, so a
        # setting in run.config.bat does not leak into another configuration.
        foreach ($v in @('VICTORY_HOT_WINDOWS_MB', 'VICTORY_RESIDENCY', 'VICTORY_PROMOTE', 'VICTORY_RELAY', 'VICTORY_LATE_WAIT',
                'VICTORY_CARD_BUFFERS_MIN_MB', 'VICTORY_CARD_BUFFERS_MB', 'VICTORY_WIDE_REFRESH', 'VICTORY_WIDE_FILLS', 'VICTORY_KEEP_SNAPSHOTS', 'VICTORY_BACK_NOTE',
                'VICTORY_TEXTURE_FROM_COPY', 'VICTORY_PLAIN_COPY', 'VICTORY_WATCHED_ALLOC', 'VICTORY_HOT_SKIP', 'CONSOLE', 'VICTORY_FFX_SIMPLE')) {
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
        $stopped = -not $p.HasExited
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
        $runs.Add([pscustomobject]@{ Name = $name; Overrides = $over; Run = (Read-Run $log.FullName); Seconds = $ran; Stopped = $stopped })
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
