# Runs one program for run.bat: shows its output, appends it to a UTF-8 log,
# and exits with the program's own exit code.
#
# Input comes from the environment so that no argument goes through two
# rounds of shell quoting:
#   SIXTH_PROGRAM  the program (a path, or a name on PATH)
#   SIXTH_ARGS     its arguments, separated by '|' (never part of a Windows path)
#   SIXTH_LOG      the log file (appended to)
$ErrorActionPreference = 'Continue'
$program = $env:SIXTH_PROGRAM
$arguments = @()
if ($env:SIXTH_ARGS) { $arguments = $env:SIXTH_ARGS -split '\|' }
$log = New-Object IO.StreamWriter($env:SIXTH_LOG, $true, (New-Object Text.UTF8Encoding($false)))
$log.AutoFlush = $true
$rc = 1
try {
    $log.WriteLine("[run] > $program $($arguments -join ' ')")
    # 2>&1 turns stderr lines into error records; "$_" is their plain text.
    & $program @arguments 2>&1 | ForEach-Object { $line = "$_"; $log.WriteLine($line); $line }
    if ($null -ne $LASTEXITCODE) { $rc = $LASTEXITCODE }
} catch {
    $line = "[run] could not start ${program}: $($_.Exception.Message)"
    $log.WriteLine($line)
    Write-Host $line
} finally {
    $log.WriteLine(('[run] exit code {0} (0x{0:X8})' -f $rc))
    $log.Close()
}
exit $rc
