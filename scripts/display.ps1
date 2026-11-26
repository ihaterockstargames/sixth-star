# The display mode the title is told when DISPLAY_MODE is empty: the output
# (OUTPUT_MODE, or 1920x1080) times RENDER_SCALE, rounded and clamped as
# crates/victory-gpu/src/resolution.rs does it. For run.bat doctor only; the
# runner computes and logs the value it uses.
param([string]$Output = '', [string]$Scale = '1.0')

if (-not $Output) { $Output = '1920x1080' }
$parts = $Output -split 'x'
$culture = [Globalization.CultureInfo]::InvariantCulture
$s = 0.0
if ($parts.Count -ne 2 -or -not [double]::TryParse($Scale, [Globalization.NumberStyles]::Float, $culture, [ref]$s) -or $s -lt 0.1 -or $s -gt 2.0) {
    $s = 1.0
}
function Scaled([double]$v, [int]$lo, [int]$hi) {
    [math]::Min($hi, [math]::Max($lo, [math]::Round($v * $s, [MidpointRounding]::AwayFromZero)))
}
'{0}x{1} (output x scale)' -f (Scaled ([double]$parts[0]) 640 7680), (Scaled ([double]$parts[1]) 360 4320)
