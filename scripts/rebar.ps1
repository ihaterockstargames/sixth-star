# Reports whether Resizable BAR is active on an NVIDIA GPU.
# Exit codes: 0 enabled, 1 disabled, 2 unknown / no NVIDIA GPU.
$ErrorActionPreference = 'Stop'

$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if (-not $smi) {
    Write-Output 'rebar: unknown (nvidia-smi not found; check GPU-Z or the vendor tool)'
    exit 2
}

$text = & nvidia-smi -q -d MEMORY 2>$null
if ($LASTEXITCODE -ne 0 -or -not $text) {
    Write-Output 'rebar: unknown (nvidia-smi failed)'
    exit 2
}

$lines = @($text)
$bar1 = $null
$fb = $null
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^\s*BAR1 Memory Usage\s*$') {
        for ($j = $i + 1; $j -lt [Math]::Min($i + 8, $lines.Count); $j++) {
            if ($lines[$j] -match 'Total\s*:\s*(\d+)\s*MiB') {
                $bar1 = [int]$Matches[1]
                break
            }
        }
    }
    if ($lines[$i] -match '^\s*FB Memory Usage\s*$') {
        for ($j = $i + 1; $j -lt [Math]::Min($i + 8, $lines.Count); $j++) {
            if ($lines[$j] -match 'Total\s*:\s*(\d+)\s*MiB') {
                $fb = [int]$Matches[1]
                break
            }
        }
    }
}

if ($null -eq $bar1 -or $null -eq $fb) {
    Write-Output 'rebar: unknown (could not read BAR1/FB memory)'
    exit 2
}

Write-Output ("rebar: BAR1 {0} MiB / VRAM {1} MiB" -f $bar1, $fb)
if ($bar1 -ge $fb) {
    Write-Output 'rebar: enabled'
    exit 0
}

Write-Output 'rebar: disabled'
Write-Output 'rebar: enable Above 4G Decoding and Re-Size BAR Support in the BIOS,'
Write-Output 'rebar: set CSM to Disabled, save, reboot; then run: run.bat rebar'
exit 1
