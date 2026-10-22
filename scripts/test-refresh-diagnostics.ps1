[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$LogPath,
    [string[]]$ForbiddenSentinel = @(),
    [string[]]$RequiredName = @(),
    [switch]$AllowIncomplete
)
$ErrorActionPreference = 'Stop'
$file = Get-Item -LiteralPath $LogPath
if ($file.Length -gt 64MB) { throw 'Diagnostic log exceeds 64 MiB' }
$raw = [IO.File]::ReadAllText($file.FullName)
foreach ($sentinel in $ForbiddenSentinel) {
    if (!$sentinel) { throw 'Sentinels must be nonempty' }
    if ($raw.IndexOf($sentinel,[StringComparison]::OrdinalIgnoreCase) -ge 0) { throw 'Private fixture sentinel leaked into diagnostic log' }
}
$spans = @{}; $names = @{}; $previous = -1L; $count = 0; $dropped = 0L; $limited = $false; $ended = $false
foreach ($line in [IO.File]::ReadLines($file.FullName)) {
    if (!$line.Trim()) { throw 'Blank JSONL record' }
    $record = $line | ConvertFrom-Json -AsHashtable
    $expected = @('v','seq','session_ms','thread','event','name','span','duration_ms','outcome','metrics')
    if (@($record.Keys | Where-Object { $_ -notin $expected }).Count -or @($expected | Where-Object { !$record.ContainsKey($_) }).Count) { throw 'Unexpected diagnostic schema fields' }
    if ($record.v -ne 1 -or $record.seq -isnot [long] -and $record.seq -isnot [int] -or $record.seq -le $previous) { throw 'Invalid version or non-increasing sequence' }
    $previous = $record.seq
    if ($record.session_ms -isnot [ValueType] -or $record.session_ms -lt 0 -or ![double]::IsFinite([double]$record.session_ms)) { throw 'Invalid timestamp' }
    if ($record.thread -notmatch '^ThreadId\([0-9]+\)$') { throw 'Unexpected thread identifier' }
    if ($record.event -notin @('begin','end','event') -or $record.name -cnotmatch '^[a-z][a-z0-9_]*$') { throw 'Invalid event or operation name' }
    if ($null -ne $record.span -and ($record.span -isnot [long] -and $record.span -isnot [int] -or $record.span -lt 0)) { throw 'Invalid span identifier' }
    if ($null -ne $record.outcome -and $record.outcome -cnotmatch '^[a-z][a-z0-9_]*$') { throw 'Invalid outcome' }
    if ($record.metrics -isnot [Collections.IDictionary]) { throw 'Metrics must be an object' }
    foreach ($entry in $record.metrics.GetEnumerator()) {
        if ($entry.Key -cnotmatch '^[a-z][a-z0-9_]*$' -or $entry.Value -isnot [long] -and $entry.Value -isnot [int] -or $entry.Value -lt 0) { throw 'Metrics must contain static names and unsigned integers' }
    }
    $names[$record.name] = $true
    if ($record.event -eq 'begin') {
        if ($null -eq $record.span -or $spans.ContainsKey([string]$record.span)) { throw 'Duplicate or missing span' }
        $spans[[string]$record.span] = $record.name
    } elseif ($record.event -eq 'end') {
        $key = [string]$record.span
        if (!$spans.ContainsKey($key) -or $spans[$key] -ne $record.name) {
            if (!$AllowIncomplete) { throw 'End has no matching begin' }
        } else { $spans.Remove($key) }
        if ($null -eq $record.outcome -or $null -eq $record.duration_ms -or $record.duration_ms -lt 0 -or ![double]::IsFinite([double]$record.duration_ms)) { throw 'End lacks valid duration or outcome' }
    }
    if ($record.name -eq 'session_end') {
        if (!$record.metrics.Contains('dropped_events')) { throw 'Session end lacks dropped event count' }
        $ended = $true; $dropped = [long]$record.metrics.dropped_events
    }
    if ($record.name -eq 'log_limit_reached') { $limited = $true }
    $count++
}
if (!$count -or !$names.ContainsKey('session_start')) { throw 'Missing session start' }
foreach ($name in $RequiredName) { if (!$names.ContainsKey($name)) { throw "Missing required operation: $name" } }
if (!$AllowIncomplete -and (!$ended -or $spans.Count -or $dropped -or $limited)) { throw 'Incomplete session: missing end, unmatched spans, dropped events, or size limit; inspect with -AllowIncomplete' }
[pscustomobject]@{records=$count;unmatchedSpans=$spans.Count;droppedEvents=$dropped;sizeLimited=$limited;sessionEnded=$ended;bytes=$file.Length}
