param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
# Reuse established startup/native-frame parsing, then append this bundle's
# operation-specific results. No measurements or builds run in this script.
$rows = @()
if (@('startup','history','actions') | Where-Object { Test-Path -LiteralPath (Join-Path $OutputDirectory "$_/samples.json") }) {
    & "$PSScriptRoot/summarize-app-improvements.ps1" -OutputDirectory $OutputDirectory | Out-Null
    $rows = @(Get-Content (Join-Path $OutputDirectory 'summary.json') -Raw | ConvertFrom-Json | Where-Object { $null -ne $_ })
}
function Percentile($Values,[double]$p) {
    $v = @($Values | Sort-Object)
    if (!$v.Count) { throw 'Missing timing samples' }
    return $v[[Math]::Ceiling($v.Count*$p)-1]
}
function Metric([string]$Name,$Before,$After) {
    $b = @($Before | Where-Object { $null -ne $_ }); $c = @($After | Where-Object { $null -ne $_ })
    if (!$c.Count) { throw "Missing candidate samples: $Name" }
    $bm = if ($b.Count) { Percentile $b .5 } else { $null }
    $bp = if ($b.Count) { Percentile $b .95 } else { $null }
    $cm = Percentile $c .5; $cp = Percentile $c .95
    return [pscustomobject]@{
        metric=$Name;baselineSamples=$b.Count;candidateSamples=$c.Count
        baselineMedianMs=$bm;candidateMedianMs=$cm;baselineP95Ms=$bp;candidateP95Ms=$cp
        medianDeltaMs=if($b.Count){$cm-$bm}else{$null}
        medianDeltaPercent=if($b.Count -and $bm -gt 0){100*($cm-$bm)/$bm}else{$null}
        p95DeltaPercent=if($b.Count -and $bp -gt 0){100*($cp-$bp)/$bp}else{$null}
    }
}
function Read-HarnessRows([string]$Path) {
    $text = Get-Content -LiteralPath $Path -Raw
    if ($text -notmatch 'test result: ok\. 1 passed;') { throw "Unsuccessful harness: $Path" }
    return @([regex]::Matches($text,'(?m)^\{[^\r\n]*\}\r?$') | ForEach-Object { $_.Value | ConvertFrom-Json })
}
$toolbar = Join-Path $OutputDirectory 'actions/samples.json'
if (Test-Path -LiteralPath $toolbar) {
    $samples = @(Get-Content -LiteralPath $toolbar -Raw | ConvertFrom-Json | Where-Object scenario -eq 'toolbar-counts')
    if ($samples.Count) {
        $cpu = @($samples | ForEach-Object { $_.toolbarHandlerMs } | Where-Object { $null -ne $_ })
        if ($cpu.Count -ne 24*$samples.Count) { throw 'Expected 24 actual toolbar transitions per measured launch' }
        $rows += Metric 'toolbar-counts-handler-cpu' @() $cpu
        $means = @($samples | ForEach-Object { ($_.toolbarHandlerMs | Measure-Object -Average).Average })
        $rows += Metric 'toolbar-counts-handler-launch-means' @() $means
    }
}
$nativeTags = Join-Path $OutputDirectory 'tags-native/samples.json'
if (Test-Path -LiteralPath $nativeTags) {
    $samples = @(Get-Content -LiteralPath $nativeTags -Raw | ConvertFrom-Json)
    foreach ($group in ($samples | Group-Object scenario)) {
        $rows += Metric "$($group.Name)-action-to-frame" @() $group.Group.actionToFrameMs
    }
}
$components = Join-Path $OutputDirectory 'components'
$engineName = 'engine-tests-benchmark_desktop_mutations.txt'
if (Test-Path -LiteralPath (Join-Path $components "candidate-$engineName")) {
    $b = @(Read-HarnessRows (Join-Path $components "baseline-$engineName"))
    $c = @(Read-HarnessRows (Join-Path $components "candidate-$engineName"))
    if ($b.Count -ne 15 -or $c.Count -ne 15) { throw 'Expected 15 mutation samples per executable' }
    foreach ($field in @('statusAndHistoryMs','commitMs','checkoutMs')) {
        $rows += Metric "mutations-$field" $b.$field $c.$field
    }
}
$journalName = 'git-operations-tests-benchmark_large_worktree_operations.txt'
if (Test-Path -LiteralPath (Join-Path $components "candidate-$journalName")) {
    $before = @(Read-HarnessRows (Join-Path $components "baseline-$journalName"))
    $after = @(Read-HarnessRows (Join-Path $components "candidate-$journalName"))
    foreach ($dirty in @($false,$true)) {
        $b = @($before | Where-Object { $_.dirty -eq $dirty }); $c = @($after | Where-Object { $_.dirty -eq $dirty })
        if ($b.Count -ne 15 -or $c.Count -ne 15) { throw 'Expected 15 journal samples per executable/worktree mode' }
        $mode = if ($dirty) { 'dirty' } else { 'clean' }
        foreach ($field in @('create_ms','undo_ms','redo_ms')) { $rows += Metric "journal-$mode-$field" $b.$field $c.$field }
    }
}
$tags = Join-Path $components 'candidate-tags.txt'
if (Test-Path -LiteralPath $tags) {
    $samples = @(Read-HarnessRows $tags)
    foreach ($annotated in @($false,$true)) {
        $s = @($samples | Where-Object { $_.annotated -eq $annotated })
        if ($s.Count -ne 15) { throw 'Expected 15 samples per tag type' }
        $kind = if ($annotated) { 'annotated' } else { 'lightweight' }
        foreach ($field in @('create_ms','undo_ms','redo_ms')) { $rows += Metric "tag-$kind-$field" @() $s.$field }
    }
}
foreach ($row in $rows) {
    $p95Delta = if ($row.baselineSamples) { $row.candidateP95Ms-$row.baselineP95Ms } else { $null }
    $row | Add-Member -NotePropertyName p95DeltaMs -NotePropertyValue $p95Delta -Force
}
$rows | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory 'summary.json')
$lines = @('| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta ms / % | p95 delta ms / % | Samples B/C |',
    '| --- | ---: | ---: | ---: | ---: | ---: |')
foreach ($row in $rows) {
    $b = if ($row.baselineSamples) { '{0:F4}/{1:F4}' -f $row.baselineMedianMs,$row.baselineP95Ms } else { 'candidate only' }
    $md = if ($row.baselineSamples) { '{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.medianDeltaMs,$row.medianDeltaPercent } else { 'n/a' }
    $pd = if ($row.baselineSamples) { '{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.p95DeltaMs,$row.p95DeltaPercent } else { 'n/a' }
    $lines += '| {0} | {1} | {2:F4}/{3:F4} | {4} | {5} | {6}/{7} |' -f $row.metric,$b,$row.candidateMedianMs,$row.candidateP95Ms,$md,$pd,$row.baselineSamples,$row.candidateSamples
}
$lines += ''
$lines += 'Nearest-rank percentiles; 15-launch p95 is the maximum. Frame and transition values within a launch are correlated: inspect launch means too. Native shared launches alternate binary order; component harnesses retain their own warm-up/15 samples and run as separate binary blocks, reversing order between tests. Tag/UI scenarios without baseline instrumentation are candidate only. Dialog timing is open-to-frame; create timing is submit-to-refreshed-tag-label frame. Toolbar handler CPU is separate from 120 rendered frame intervals (24 real transitions per launch). Physical input, display scanout and real provider/network latency are excluded. Missing stages are omitted rather than treated as zero; this table alone does not certify suite completion.'
$lines | Set-Content (Join-Path $OutputDirectory 'summary.md')
$rows | Format-Table metric,baselineMedianMs,candidateMedianMs,baselineP95Ms,candidateP95Ms,medianDeltaPercent
