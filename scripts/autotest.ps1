# The autonomous test runner. Each cycle:
#
#   build    the release binary, a diagnostics binary (frame capture) in its
#            own target folder, and the stand-in title (tests\fixtures\test-title)
#   tests    the whole workspace's tests (GPU required)
#   bench    synthetic benchmarks through the runtime's own paths
#            (synthetic_ab, pcie_contention_bench)
#   title    the stand-in title through the real sixth-star.exe, once per
#            configuration: started, watched (a run that stops writing is a
#            hang and is killed), its exit code and crash log read, run again
#            once on a failure, its frame time and the runtime's telemetry
#            read (ab.ps1 -ParseOnly -Json), and its screenshots checked
#            pixel by pixel against the picture the title draws
#   compare  against the previous cycle: faster, slower or the same, and
#            anything that broke
#
# Progress is saved in <Root>\state.json after every stage and every title
# run: a cycle that was interrupted (a reboot, a closed window) goes on from
# where it stopped the next time this runs. Each cycle's files are in
# <Root>\cycles\<n>; <Root>\latest.md is the last report, and
# <Root>\history.jsonl holds one line per finished cycle.
#
# One runner at a time: while one runs, <Root>\autotest.lock is held, and a
# second runner (a scheduled run that comes while a cycle still goes) stops
# at once with exit code 3.
#
#   autotest.bat                                one cycle, every stage
#   autotest.bat -Cycles 0 -PauseMinutes 30     cycles until stopped
#   autotest.bat -Stages title,compare          only the title runs and the comparison
#
# It runs no title but the stand-in it builds itself, and it does not read
# run.config.bat: the caches and data of its runs are kept in <Root>.
[CmdletBinding()]
param(
    # Where state, logs and reports go (default: autotest next to the repository).
    [string]$Root = '',
    # Cycles to run; 0 = until stopped.
    [int]$Cycles = 1,
    # Minutes between cycles.
    [int]$PauseMinutes = 0,
    [string[]]$Stages = @('build', 'tests', 'bench', 'title', 'compare'),
    # The configurations of the title runs (ab.ps1's names).
    [string[]]$Configs = @('baseline', 'resid', 'texcopy', 'hot1024', 'cardbuf1', 'best'),
    # How long each title run lasts.
    [int]$TitleSeconds = 45,
    # Buffers the stand-in title rewrites and copies each frame.
    [int]$TitleCopies = 8,
    # Runs of a configuration after a failure.
    [int]$Retries = 1,
    # Frames of synthetic_ab's frame workload (its streaming one runs twice as many).
    [int]$AbFrames = 150,
    # Start a new cycle even if the last one did not finish.
    [switch]$Fresh
)
$ErrorActionPreference = 'Stop'
# From a .bat (powershell -File) a list arrives as one string: split it.
$Stages = @($Stages | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$Configs = @($Configs | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Root) { $Root = Join-Path (Split-Path -Parent $repo) 'autotest' }
New-Item -ItemType Directory -Force $Root | Out-Null
$Root = (Resolve-Path $Root).Path
$statePath = Join-Path $Root 'state.json'
$historyPath = Join-Path $Root 'history.jsonl'
$runnerLog = Join-Path $Root 'autotest.log'
$diagnosticsDir = Join-Path $Root 'target-diag'
$diagExe = Join-Path $diagnosticsDir 'release\sixth-star.exe'
$titleTarget = Join-Path $Root 'target-title'
$titleExe = Join-Path $titleTarget 'release\test-title.exe'
$titleManifest = Join-Path $repo 'tests\fixtures\test-title\Cargo.toml'
$ab = Join-Path $PSScriptRoot 'ab.ps1'
$allStages = @('build', 'tests', 'bench', 'title', 'compare')
# Every switch a configuration may set: cleared before each title run.
$switches = @('VICTORY_HOT_WINDOWS_MB', 'VICTORY_RESIDENCY', 'VICTORY_PROMOTE', 'VICTORY_RELAY', 'VICTORY_LATE_WAIT',
    'VICTORY_CARD_BUFFERS_MIN_MB', 'VICTORY_CARD_BUFFERS_MB', 'VICTORY_TEXTURE_FROM_COPY', 'VICTORY_WIDE_REFRESH', 'VICTORY_WIDE_FILLS',
    'VICTORY_WIDE_BACK', 'VICTORY_KEEP_SNAPSHOTS', 'VICTORY_BACK_NOTE')
# The stand-in title's picture (tests\fixtures\test-title\src\main.rs): a
# blue band over an 8 x 4 grid, block (r, c) in PALETTE[(r * 3 + c) % 8].
$palette = @(@(230, 25, 75), @(60, 180, 75), @(255, 225, 25), @(0, 130, 200), @(245, 130, 48), @(145, 30, 180), @(70, 240, 240), @(240, 50, 230))

function Say([string]$text) {
    $line = "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $text"
    Write-Host $line
    Add-Content -Path $runnerLog -Value $line -Encoding UTF8
}

# ---- state ----------------------------------------------------------------------
function ConvertTo-Hash($o) {
    if ($null -eq $o) { return $null }
    if ($o -is [System.Management.Automation.PSCustomObject]) {
        $h = [ordered]@{}
        foreach ($p in $o.PSObject.Properties) { $h[$p.Name] = ConvertTo-Hash $p.Value }
        return $h
    }
    # The comma keeps a list of one a list: returned bare, PowerShell
    # unrolls it, and a run's one attempt came back as the attempt itself
    # (so `attempts[-1]` of a resumed cycle or of the last cycle in
    # history.jsonl was nothing, and the comparison compared nothing).
    if ($o -is [System.Collections.IEnumerable] -and $o -isnot [string]) { return , @($o | ForEach-Object { ConvertTo-Hash $_ }) }
    return $o
}
function Save-State($state) {
    $tmp = "$statePath.tmp"
    [IO.File]::WriteAllText($tmp, (ConvertTo-Json -InputObject $state -Depth 12), (New-Object Text.UTF8Encoding($false)))
    Move-Item -Force $tmp $statePath
}
function Load-State {
    if (-not (Test-Path $statePath)) { return $null }
    try { return ConvertTo-Hash (Get-Content -Raw $statePath | ConvertFrom-Json) } catch { Say "state unreadable, starting over: $_"; return $null }
}

# ---- running programs -------------------------------------------------------------
# Runs `file args` with `env` added, its output (both streams) to `log`; kills
# the whole tree after `timeout` seconds. Returns @{ code; seconds; timedOut }.
function Invoke-Logged([string]$file, [string]$arguments, [string]$log, [hashtable]$vars = @{}, [int]$timeout = 3600, [string]$cwd = $repo) {
    $saved = @{}
    foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
    try {
        $started = Get-Date
        $cmd = "`"$file`" $arguments > `"$log`" 2>&1"
        $p = Start-Process -FilePath $env:ComSpec -ArgumentList '/d', '/s', '/c', "`"$cmd`"" -WorkingDirectory $cwd -NoNewWindow -PassThru
        # The exit code is only kept if the handle is taken while the process runs.
        $null = $p.Handle
        $timedOut = -not $p.WaitForExit($timeout * 1000)
        if ($timedOut) { & taskkill /T /F /PID $p.Id 2>$null | Out-Null; $null = $p.WaitForExit(30000) }
        return @{ code = $(if ($timedOut) { -1 } else { $p.ExitCode }); seconds = ((Get-Date) - $started).TotalSeconds; timedOut = $timedOut }
    } finally {
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
    }
}

# ---- stages -------------------------------------------------------------------------
function Stage-Build($dir) {
    $r = [ordered]@{ ok = $true; steps = [ordered]@{} }
    $r.commit = (& git -C $repo rev-parse --short HEAD 2>$null)
    $r.dirty = [bool](& git -C $repo status --porcelain --untracked-files=no 2>$null)
    $builds = [ordered]@{
        plain = 'build --release -p victory'
        diagnostics = "build --release -p victory --features diagnostics --target-dir `"$diagnosticsDir`""
        title = "build --release --manifest-path `"$titleManifest`" --target-dir `"$titleTarget`""
    }
    foreach ($name in $builds.Keys) {
        $x = Invoke-Logged 'cargo' $builds[$name] (Join-Path $dir "build-$name.log") @{} 3600
        $r.steps[$name] = [ordered]@{ code = $x.code; seconds = [math]::Round($x.seconds, 1) }
        if ($x.code -ne 0) { $r.ok = $false; Say "build $name failed (exit $($x.code)); see build-$name.log" }
    }
    return $r
}

function Stage-Tests($dir) {
    $log = Join-Path $dir 'tests.log'
    $x = Invoke-Logged 'cargo' 'test --workspace -q' $log @{ SIXTH_REQUIRE_GPU = '1' } 7200
    $passed = 0; $failed = 0; $ignored = 0
    foreach ($m in (Select-String -Path $log -Pattern 'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored' -AllMatches)) {
        foreach ($g in $m.Matches) { $passed += [int]$g.Groups[1].Value; $failed += [int]$g.Groups[2].Value; $ignored += [int]$g.Groups[3].Value }
    }
    $failures = @(Select-String -Path $log -Pattern '^test .* FAILED$|panicked at' | Select-Object -First 10 | ForEach-Object { $_.Line })
    return [ordered]@{ ok = ($x.code -eq 0 -and $failed -eq 0); code = $x.code; passed = $passed; failed = $failed; ignored = $ignored; seconds = [math]::Round($x.seconds, 0); failures = $failures }
}

function Stage-Bench($dir) {
    $r = [ordered]@{ ok = $true }
    $report = Join-Path $dir 'synthetic-ab.md'
    $x = Invoke-Logged 'cargo' 'test -p victory-gpu --release --lib synthetic_ab -- --ignored --nocapture' (Join-Path $dir 'synthetic-ab.log') @{ SIXTH_AB_FRAMES = "$AbFrames"; SIXTH_AB_REPORT = $report } 3600
    $rows = [ordered]@{}
    if (Test-Path $report) {
        foreach ($line in Get-Content $report) {
            if ($line -match '^\| (\w+) \| ([\d.]+) \(([\d.]+), ([\d.]+)\) \| [^|]+\| ([\d.]+) \(([\d.]+), ([\d.]+)\) \| [^|]+\| (yes|\*\*NO\*\*)') {
                $rows[$Matches[1]] = [ordered]@{ frameMs = [double]$Matches[2]; frameP95 = [double]$Matches[3]; streamMs = [double]$Matches[5]; streamP95 = [double]$Matches[6]; correct = ($Matches[8] -eq 'yes') }
            }
        }
    }
    $r.syntheticAb = $rows
    if ($x.code -ne 0 -or $rows.Count -eq 0 -or @($rows.Values | Where-Object { -not $_.correct }).Count) { $r.ok = $false }
    $c = Invoke-Logged 'cargo' 'test -p victory-gpu --release --lib pcie_contention_bench -- --ignored --nocapture' (Join-Path $dir 'contention.log') @{} 1800
    $row = Select-String -Path (Join-Path $dir 'contention.log') -Pattern 'CONTENTION-ROW ([\d.|]+)' | Select-Object -First 1
    if ($row) {
        $v = $row.Matches[0].Groups[1].Value -split '\|'
        $r.contention = [ordered]@{ cpuAlone = [double]$v[0]; cpuWithCard = [double]$v[1]; cpuWithPc = [double]$v[2]; gpuPc = [double]$v[3]; gpuPcUnderCpu = [double]$v[4]; gpuCard = [double]$v[5] }
    } else { $r.ok = $false }
    return $r
}

# The picture's sample points (in the 1280 x 720 target) and colours.
function Get-Expected {
    $points = @(@{ x = 640; y = 60; rgb = @(0, 0, 255); what = 'band' })
    for ($row = 0; $row -lt 4; $row++) {
        for ($col = 0; $col -lt 8; $col++) {
            $points += @{ x = $col * 160 + 80; y = 120 + $row * 150 + 70; rgb = $palette[($row * 3 + $col) % 8]; what = "block $row,$col" }
        }
    }
    return $points
}

# Checks a saved frame: the colour at every sample point within 40 per
# channel. Returns the mismatches (none: the picture is right).
function Test-Picture([string]$png) {
    Add-Type -AssemblyName System.Drawing
    $bad = New-Object System.Collections.Generic.List[string]
    $bmp = [System.Drawing.Bitmap]::FromFile($png)
    try {
        $sx = $bmp.Width / 1280.0; $sy = $bmp.Height / 720.0
        foreach ($p in (Get-Expected)) {
            $c = $bmp.GetPixel([int]($p.x * $sx), [int]($p.y * $sy))
            $d = [math]::Max([math]::Abs($c.R - $p.rgb[0]), [math]::Max([math]::Abs($c.G - $p.rgb[1]), [math]::Abs($c.B - $p.rgb[2])))
            if ($d -gt 40) { $bad.Add("$($p.what): $($c.R),$($c.G),$($c.B) instead of $($p.rgb -join ',')") }
        }
    } finally { $bmp.Dispose() }
    return , $bad
}

function Invoke-Title([string]$config, [string]$dir) {
    $over = ConvertTo-Hash ((& powershell -NoProfile -ExecutionPolicy Bypass -File $ab -Overrides $config) | ConvertFrom-Json)
    $result = [ordered]@{ config = $config; settings = $over; attempts = @() }
    for ($attempt = 1; $attempt -le 1 + $Retries; $attempt++) {
        # A named configuration (`name:VAR=v;VAR=v`) runs in a folder of its name.
        $run = Join-Path $dir "$($config -replace ':.*$', '')-$attempt"
        if (Test-Path $run) { Remove-Item -Recurse -Force $run }
        New-Item -ItemType Directory -Force (Join-Path $run 'frames') | Out-Null
        Copy-Item $titleExe $run
        $vars = @{
            NO_COLOR = '1'; VICTORY_CACHE_DIR = (Join-Path $Root 'cache'); VICTORY_DATA_DIR = (Join-Path $Root 'data')
            VICTORY_CAPTURE = (Join-Path $run 'frames'); TEST_TITLE_SECONDS = "$TitleSeconds"; TEST_TITLE_COPIES = "$TitleCopies"
        }
        foreach ($s in $switches) { $vars[$s] = $null }
        foreach ($k in $over.Keys) { $vars[$k] = "$($over[$k])" }
        $log = Join-Path $run 'title.log'
        $crashLog = Join-Path $Root 'data\00000000\crashes.log'
        $crashesBefore = if (Test-Path $crashLog) { (Get-Item $crashLog).Length } else { 0 }
        # Started here and watched: a run whose log stops growing for 30 s
        # (no frame of the title's for that long) is a hang and is killed.
        $saved = @{}
        foreach ($k in $vars.Keys) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
        try {
            $cmd = "`"$diagExe`" run --execute `"$(Join-Path $run 'test-title.exe')`" > `"$log`" 2>&1"
            $started = Get-Date
            $p = Start-Process -FilePath $env:ComSpec -ArgumentList '/d', '/s', '/c', "`"$cmd`"" -WorkingDirectory $run -NoNewWindow -PassThru
            # The exit code is only kept if the handle is taken while the process runs.
            $null = $p.Handle
            $outcome = $null; $lastSize = -1; $lastGrowth = Get-Date
            while (-not $p.HasExited) {
                Start-Sleep -Seconds 2
                $size = if (Test-Path $log) { (Get-Item $log).Length } else { 0 }
                if ($size -ne $lastSize) { $lastSize = $size; $lastGrowth = Get-Date }
                if (((Get-Date) - $lastGrowth).TotalSeconds -gt 30) { $outcome = 'hang'; break }
                if (((Get-Date) - $started).TotalSeconds -gt $TitleSeconds + 120) { $outcome = 'timeout'; break }
            }
            if ($outcome) { & taskkill /T /F /PID $p.Id 2>$null | Out-Null; $null = $p.WaitForExit(30000) }
            $seconds = ((Get-Date) - $started).TotalSeconds
            $p.WaitForExit()
            $code = if ($outcome) { -1 } else { $p.ExitCode }
        } finally {
            foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
        }
        $a = [ordered]@{ attempt = $attempt; code = $code; seconds = [math]::Round($seconds, 1); outcome = $outcome }
        $text = if (Test-Path $log) { Get-Content -Raw $log } else { '' }
        if ($text -match 'TEST-TITLE done frames=(\d+) us=(\d+) us_per_frame=(\d+) worst_us=(\d+)') {
            $a.frames = [int64]$Matches[1]; $a.usPerFrame = [int64]$Matches[3]; $a.worstUs = [int64]$Matches[4]
        }
        $fps = @([regex]::Matches($text, 'TEST-TITLE second frames=\d+ fps_x100=(\d+)') | ForEach-Object { [double]$_.Groups[1].Value / 100 })
        if ($fps.Count -gt 2) {
            $steady = @($fps | Select-Object -Skip 1 | Sort-Object)
            $a.fpsMedian = $steady[[int](($steady.Count - 1) / 2)]; $a.fpsLowest = $steady[0]
        }
        if ($text -match 'TEST-TITLE failed step (\d+) hr=(0x[0-9a-f]+)') { $a.failedStep = [int]$Matches[1]; $a.hr = $Matches[2] }
        $crashesAfter = if (Test-Path $crashLog) { (Get-Item $crashLog).Length } else { 0 }
        $a.crashLogged = $crashesAfter -gt $crashesBefore
        if (-not $a.outcome) {
            $a.outcome = if ($code -eq 0 -and $a.frames) { 'ok' } elseif ($a.crashLogged -or $code -ne 0) { 'crash' } else { 'incomplete' }
        }
        # The runtime's own lines (frame line, GPU, VRAM, RAM), all frames.
        try {
            & powershell -NoProfile -ExecutionPolicy Bypass -File $ab -ParseOnly $log -Configs $config -MinDraws 0 -SkipSeconds 5 -OutDir $run -Json *> (Join-Path $run 'parse.txt')
            $parsed = Get-Content -Raw (Join-Path $run 'ab-report.json') | ConvertFrom-Json
            $rt = @($parsed)[0]
            $a.runtime = [ordered]@{
                msPerFrame = $rt.median.Ms; preparing = $rt.median.Cpu; copiesMs = $rt.median.CopyMs; copiesMb = $rt.median.CopyMb
                gpu = $rt.median.Gpu; gpuWork = $rt.gpu.Title; backMs = $rt.median.BackMs; vramUsed = $rt.vram.Used; ramFree = $rt.vram.RamFree
                committed = $rt.vram.Private; workingSet = $rt.vram.WorkingSet; faults = $rt.vram.Faults; cores = $rt.vram.Cores
                errors = $rt.errors; removed = $rt.removed; panics = $rt.panics; watchdog = $rt.watchdog
            }
        } catch { $a.runtime = $null; Say "  $config`: the runtime's lines were not read: $_" }
        # The screenshots from frame 8 on, the last four of them.
        $frames = @(Get-ChildItem (Join-Path $run 'frames') -Filter 'frame-*.png' -ErrorAction SilentlyContinue |
                Where-Object { $_.BaseName -match '^frame-(\d+)$' -and [int64]$Matches[1] -ge 8 } |
                Sort-Object { [int64]($_.BaseName -replace '^frame-', '') } | Select-Object -Last 4)
        $mismatches = @()
        foreach ($f in $frames) { foreach ($m in (Test-Picture $f.FullName)) { $mismatches += "$($f.Name): $m" } }
        $a.picture = [ordered]@{ checked = $frames.Count; mismatches = $mismatches.Count; first = @($mismatches | Select-Object -First 5) }
        if ($a.outcome -eq 'ok' -and $frames.Count -eq 0) { $a.picture.note = 'no frame saved' }
        $result.attempts += $a
        Say ("  {0} attempt {1}: {2}, {3} frames, {4} us/frame (worst {5}), picture {6}/{7} wrong" -f $config, $attempt, $a.outcome, $a.frames, $a.usPerFrame, $a.worstUs, $a.picture.mismatches, $a.picture.checked)
        if ($a.outcome -eq 'ok' -and $a.picture.mismatches -eq 0 -and $frames.Count -gt 0) { break }
        if ($attempt -le $Retries) { Say "  $config`: running it again (restart after $($a.outcome))" }
    }
    $last = $result.attempts[-1]
    $result.ok = ($last.outcome -eq 'ok' -and $last.picture.mismatches -eq 0 -and $last.picture.checked -gt 0)
    $result.restarted = $result.attempts.Count -gt 1
    return $result
}

function Stage-Compare($cycle) {
    $prev = $null
    if (Test-Path $historyPath) {
        $lines = @(Get-Content $historyPath | Where-Object { $_.Trim() })
        if ($lines.Count) { $prev = ConvertTo-Hash ($lines[-1] | ConvertFrom-Json) }
    }
    $r = [ordered]@{ against = $(if ($prev) { $prev.cycle } else { $null }); changes = @(); broken = @() }
    $titles = $cycle.results.title
    if ($titles) {
        foreach ($name in $titles.Keys) {
            $t = $titles[$name]
            if (-not $t.ok) { $r.broken += "title $name`: $(@($t.attempts)[-1].outcome), picture $(@($t.attempts)[-1].picture.mismatches) wrong" }
            if ($t.restarted) { $r.broken += "title $name`: needed a restart" }
            $now = @($t.attempts)[-1]
            $old = if ($prev -and $prev.results.title -and $prev.results.title[$name]) { @($prev.results.title[$name].attempts)[-1] } else { $null }
            if ($old -and $old.usPerFrame -and $now.usPerFrame) {
                $d = ($now.usPerFrame - $old.usPerFrame) / $old.usPerFrame * 100
                if ([math]::Abs($d) -ge 10) { $r.changes += ("title {0}: {1:+0;-0}% time per frame ({2} -> {3} us)" -f $name, $d, $old.usPerFrame, $now.usPerFrame) }
            }
            if ($old -and $old.runtime -and $now.runtime) {
                foreach ($k in @('preparing', 'gpuWork', 'backMs')) {
                    $o = $old.runtime[$k]; $n = $now.runtime[$k]
                    if ($null -eq $o -or $null -eq $n) { continue }
                    # As doubles: for a whole 0 (no copying back) [math]::Max
                    # took its integer form, 0.1 became 0 and this divided by it.
                    $d = ([double]$n - [double]$o) / [math]::Max([double]$o, 0.1) * 100
                    if ([math]::Abs($d) -ge 15 -and [math]::Abs($n - $o) -ge 0.3) { $r.changes += ("title {0}: {1:+0;-0}% {2} ({3} -> {4} ms)" -f $name, $d, $k, $o, $n) }
                }
            }
        }
    }
    $ab = $cycle.results.bench.syntheticAb
    $oldAb = if ($prev -and $prev.results.bench) { $prev.results.bench.syntheticAb } else { $null }
    if ($ab) {
        foreach ($name in $ab.Keys) {
            if (-not $ab[$name].correct) { $r.broken += "synthetic_ab $name`: a byte was wrong" }
            if ($oldAb -and $oldAb[$name]) {
                foreach ($k in @('frameMs', 'streamMs')) {
                    $o = $oldAb[$name][$k]; $n = $ab[$name][$k]
                    if ($o -and $n) {
                        $d = ($n - $o) / $o * 100
                        if ([math]::Abs($d) -ge 10) { $r.changes += ("synthetic_ab {0} {1}: {2:+0;-0}% ({3} -> {4} ms)" -f $name, $k, $d, $o, $n) }
                    }
                }
            }
        }
    }
    if ($cycle.results.tests -and -not $cycle.results.tests.ok) { $r.broken += "tests: $($cycle.results.tests.failed) failed" }
    if ($cycle.results.build -and -not $cycle.results.build.ok) { $r.broken += 'build failed' }
    return $r
}

function Write-CycleReport($cycle, [string]$dir) {
    $o = New-Object System.Collections.Generic.List[string]
    $o.Add("# Sixth Star autotest, cycle $($cycle.cycle)")
    $o.Add('')
    $b = $cycle.results.build
    # Without a build stage this cycle, the repository as it is now.
    $commit = if ($b) { $b.commit } else { (& git -C $repo rev-parse --short HEAD 2>$null) }
    $dirty = if ($b) { $b.dirty } else { [bool](& git -C $repo status --porcelain --untracked-files=no 2>$null) }
    $o.Add("Started $($cycle.started), finished $(Get-Date -Format 'yyyy-MM-dd HH:mm'). Commit $commit$(if ($dirty) { ' (with changes not committed)' })$(if (-not $b) { ', binaries from the last build' }). Machine: $((Get-CimInstance Win32_VideoController | Select-Object -First 1).Name); $((Get-CimInstance Win32_Processor | Select-Object -First 1).Name).")
    $o.Add('')
    $o.Add('The title runs are of the stand-in title from tests\fixtures\test-title (written for this project; no game), through the real sixth-star.exe with frame capture. Its picture is known, so every saved frame is checked pixel by pixel.')
    $o.Add('')
    if ($b) { $o.Add("- build: $(if ($b.ok) { 'ok' } else { '**FAILED**' }) ($(($b.steps.Keys | ForEach-Object { "$_ $($b.steps[$_].seconds) s" }) -join ', '))") }
    $t = $cycle.results.tests
    if ($t) { $o.Add("- tests: $(if ($t.ok) { 'ok' } else { '**FAILED**' }): $($t.passed) passed, $($t.failed) failed, $($t.ignored) ignored ($($t.seconds) s)") }
    $bench = $cycle.results.bench
    if ($bench) {
        $o.Add('')
        $o.Add("## Synthetic benchmarks ($AbFrames frames)")
        $o.Add('')
        $o.Add('| configuration | frame, ms (p95) | streaming, ms (p95) | every byte right |')
        $o.Add('|---|---|---|---|')
        foreach ($k in $bench.syntheticAb.Keys) { $x = $bench.syntheticAb[$k]; $o.Add("| $k | $($x.frameMs) ($($x.frameP95)) | $($x.streamMs) ($($x.streamP95)) | $(if ($x.correct) { 'yes' } else { '**NO**' }) |") }
        if ($bench.contention) { $c = $bench.contention; $o.Add(''); $o.Add("Contention: the CPU copies $($c.cpuAlone) GB/s alone, $($c.cpuWithPc) while the GPU reads the PC's memory ($($c.gpuPc) GB/s; the card: $($c.gpuCard)).") }
    }
    $titles = $cycle.results.title
    if ($titles) {
        $o.Add('')
        $o.Add("## Stand-in title through sixth-star.exe ($TitleSeconds s each, $TitleCopies buffers of 3 MB rewritten and copied per frame)")
        $o.Add('')
        $o.Add('The window is held to the display (60 Hz), so the frame time says little; the runtime''s own costs per frame (CPU preparing, the GPU''s work by its timestamps, copying back) are what the comparison uses.')
        $o.Add('')
        $o.Add('| configuration | outcome | FPS (lowest second) | us per frame (worst) | CPU preparing, ms | copies, MB/frame | GPU work, ms (fence wait) | copying back, ms | RAM free, GiB | picture |')
        $o.Add('|---|---|---|---|---|---|---|---|---|---|')
        foreach ($k in $titles.Keys) {
            $a = @($titles[$k].attempts)[-1]; $rt = $a.runtime
            $pic = if ($a.picture.checked -eq 0) { 'no frame' } elseif ($a.picture.mismatches -eq 0) { "right ($($a.picture.checked) frames)" } else { "**$($a.picture.mismatches) wrong**" }
            $o.Add("| $k | $($a.outcome)$(if ($titles[$k].restarted) { ' (after a restart)' }) | $($a.fpsMedian) ($($a.fpsLowest)) | $($a.usPerFrame) ($($a.worstUs)) | $(if ($rt) { $rt.preparing }) | $(if ($rt) { $rt.copiesMb }) | $(if ($rt) { "$($rt.gpuWork) ($($rt.gpu))" }) | $(if ($rt) { $rt.backMs }) | $(if ($rt) { $rt.ramFree }) | $pic |")
        }
    }
    $cmp = $cycle.results.compare
    if ($cmp) {
        $o.Add('')
        $o.Add("## Against cycle $(if ($cmp.against) { $cmp.against } else { '(none: the first)' })")
        $o.Add('')
        if ($cmp.broken.Count) { foreach ($x in $cmp.broken) { $o.Add("- **broken:** $x") } }
        if ($cmp.changes.Count) { foreach ($x in $cmp.changes) { $o.Add("- $x") } }
        if (-not $cmp.broken.Count -and -not $cmp.changes.Count) { $o.Add('- nothing broken; no change of 10% or more') }
    }
    $text = $o -join "`r`n"
    [IO.File]::WriteAllText((Join-Path $dir 'report.md'), $text, (New-Object Text.UTF8Encoding($false)))
    [IO.File]::WriteAllText((Join-Path $Root 'latest.md'), $text, (New-Object Text.UTF8Encoding($false)))
}

# ---- one runner at a time --------------------------------------------------------------
# Two runners would share state.json and the GPU. The lock is a file opened
# delete-on-close, so it goes when the runner's process ends, however it ends.
# One left behind by a power cut has no handle and can be removed; a held one
# cannot (it is shared for reading only).
$lockPath = Join-Path $Root 'autotest.lock'
function Lock-Runner {
    for ($i = 0; $i -lt 2; $i++) {
        try {
            $f = [IO.FileStream]::new($lockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read, 4096, [IO.FileOptions]::DeleteOnClose)
            $b = [Text.Encoding]::UTF8.GetBytes("pid $PID since $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
            $f.Write($b, 0, $b.Length)
            $f.Flush()
            return $f
        } catch {
            if (-not (Test-Path -LiteralPath $lockPath)) { continue }
            try { Remove-Item -LiteralPath $lockPath -Force -ErrorAction Stop } catch { return $null }
        }
    }
    return $null
}
function Get-LockHolder {
    try {
        $f = [IO.File]::Open($lockPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]'ReadWrite, Delete')
        try { return (New-Object IO.StreamReader($f)).ReadToEnd() } finally { $f.Dispose() }
    } catch { return 'not readable' }
}
$lock = Lock-Runner
if (-not $lock) {
    Say "another runner is going ($(Get-LockHolder)): this one stops"
    exit 3
}

# ---- the cycles -----------------------------------------------------------------------
try {
    $done = 0
    while ($Cycles -eq 0 -or $done -lt $Cycles) {
        $state = Load-State
        if ($Fresh -or -not $state -or $state.finished) {
            $n = if ($state) { [int]$state.cycle + 1 } else { 1 }
            $state = [ordered]@{ cycle = $n; started = (Get-Date -Format 'yyyy-MM-dd HH:mm'); stages = @($Stages); results = [ordered]@{}; finished = $false }
            Save-State $state
            Say "cycle $n begins: $($Stages -join ', ')"
        } else {
            Say "cycle $($state.cycle) goes on from where it stopped"
        }
        $Fresh = $false
        $dir = Join-Path $Root "cycles\$($state.cycle)"
        New-Item -ItemType Directory -Force $dir | Out-Null
        foreach ($stage in $allStages) {
            if ($state.stages -notcontains $stage) { continue }
            if ($state.results.Contains($stage) -and $stage -ne 'title') { continue }
            Say "stage $stage"
            switch ($stage) {
                'build' { $state.results.build = Stage-Build $dir }
                'tests' { $state.results.tests = Stage-Tests $dir }
                'bench' { $state.results.bench = Stage-Bench $dir }
                'title' {
                    if (-not (Test-Path $diagExe) -or -not (Test-Path $titleExe)) { Say 'title: no diagnostics build or no stand-in title (run the build stage)'; $state.results.title = [ordered]@{}; break }
                    if (-not $state.results.Contains('title')) { $state.results.title = [ordered]@{} }
                    $titleDir = Join-Path $dir 'title'
                    foreach ($config in $Configs) {
                        if ($state.results.title.Contains($config)) { continue }
                        $state.results.title[$config] = Invoke-Title $config $titleDir
                        Save-State $state
                    }
                }
                'compare' { $state.results.compare = Stage-Compare $state }
            }
            Save-State $state
            if ($stage -eq 'build' -and -not $state.results.build.ok) { Say 'the build failed: the cycle stops here'; break }
        }
        $state.finished = $true
        $state.ended = (Get-Date -Format 'yyyy-MM-dd HH:mm')
        Save-State $state
        Write-CycleReport $state $dir
        Add-Content -Path $historyPath -Value (ConvertTo-Json -InputObject $state -Depth 12 -Compress) -Encoding UTF8
        Say "cycle $($state.cycle) done: $(Join-Path $dir 'report.md')"
        $done++
        if ($Cycles -ne 0 -and $done -ge $Cycles) { break }
        if ($PauseMinutes -gt 0) { Say "next cycle in $PauseMinutes minutes"; Start-Sleep -Seconds ($PauseMinutes * 60) }
    }
} finally {
    $lock.Dispose()
}
