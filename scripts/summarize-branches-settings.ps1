param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
& "$PSScriptRoot/summarize-app-improvements.ps1" -OutputDirectory $OutputDirectory | Out-Null
$nativeSummary = Join-Path $OutputDirectory 'summary.json'
$rows = if ((Test-Path -LiteralPath $nativeSummary) -and (Get-Item -LiteralPath $nativeSummary).Length) {
    @(Get-Content -LiteralPath $nativeSummary -Raw | ConvertFrom-Json | Where-Object { $null -ne $_ })
} else { @() }
$rows = @($rows | Where-Object { $null -ne $_ })
function Percentile($Values,[double]$p) {
    $v = @($Values | Sort-Object)
    if (!$v.Count) { throw 'Missing measurements' }
    return $v[[Math]::Ceiling($v.Count*$p)-1]
}
function Metric([string]$Name,$Before,$After) {
    $b = @($Before | Where-Object { $null -ne $_ }); $c = @($After | Where-Object { $null -ne $_ })
    if (!$c.Count) { throw "Missing candidate measurements: $Name" }
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
function Read-Measures([string]$Path) {
    $result = @{}
    $text = Get-Content -LiteralPath $Path -Raw
    if ($text -notmatch 'test result: ok\. 1 passed;') { throw "Unsuccessful harness: $Path" }
    $records = @([regex]::Matches($text,'(?m)^\{[^\r\n]*\}\r?$') | ForEach-Object { $_.Value | ConvertFrom-Json } | Where-Object { $_.sample -ge 0 })
    if ($text -match '(?m)branch tips; one warm-up, 15 samples: (\[[^\r\n]*\])') {
        $records = @($Matches[1] | ConvertFrom-Json)
    }
    foreach ($record in $records) {
        foreach ($field in $record.PSObject.Properties | Where-Object Name -Match '(?i)ms$') {
            if ($field.Value -isnot [ValueType]) { throw "Non-numeric measurement $($field.Name): $Path" }
            $key = if ($record.entries) { "$($record.entries)-$($field.Name)" } else { $field.Name }
            $result[$key] = if ($result.ContainsKey($key)) { @($result[$key]) + $field.Value } else { @($field.Value) }
        }
    }
    foreach ($match in [regex]::Matches($text,'(?m)^(provider_[a-z0-9_]+)=(\[[^\r\n]*\])\r?$')) {
        $result[$match.Groups[1].Value] = @($match.Groups[2].Value | ConvertFrom-Json)
    }
    if (!$result.Count) { throw "No recognized component measurements: $Path" }
    foreach ($key in $result.Keys) { if ($result[$key].Count -ne 15) { throw "Expected 15 retained $key samples: $Path" } }
    return $result
}
$components = Join-Path $OutputDirectory 'components'
if (Test-Path -LiteralPath $components) {
    foreach ($candidateFile in Get-ChildItem -LiteralPath $components -Filter 'candidate-*.txt') {
        $name = $candidateFile.BaseName.Substring('candidate-'.Length)
        $c = Read-Measures $candidateFile.FullName
        $baselinePath = Join-Path $components ("baseline-"+$candidateFile.Name.Substring('candidate-'.Length))
        $b = if (Test-Path -LiteralPath $baselinePath) { Read-Measures $baselinePath } else { @{} }
        foreach ($key in $c.Keys | Sort-Object) {
            $before = if ($b.ContainsKey($key)) { @($b[$key]) } else { @() }
            $rows += Metric "$name-$key" $before @($c[$key])
        }
    }
}
$actions = Join-Path $OutputDirectory 'actions/samples.json'
if (Test-Path -LiteralPath $actions) {
    $general = @(Get-Content $actions -Raw | ConvertFrom-Json | Where-Object scenario -eq 'general-settings')
    if ($general.Count -and $general[0].generalMetrics) {
        foreach ($name in $general[0].generalMetrics.PSObject.Properties.Name) {
            $rows += Metric "general-$name" @() @($general | ForEach-Object { $_.generalMetrics.$name })
        }
    }
}
foreach ($row in $rows) {
    $row | Add-Member -NotePropertyName p95DeltaMs -NotePropertyValue $(if($row.baselineSamples){$row.candidateP95Ms-$row.baselineP95Ms}else{$null}) -Force
}
$rows | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutputDirectory 'summary.json')
$lines = @('| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta ms / % | p95 delta ms / % | Samples B/C |',
    '| --- | ---: | ---: | ---: | ---: | ---: |')
foreach ($row in $rows) {
    $before = if ($row.baselineSamples) { '{0:F4}/{1:F4}' -f $row.baselineMedianMs,$row.baselineP95Ms } else { 'candidate only' }
    $md = if ($row.baselineSamples) { '{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.medianDeltaMs,$row.medianDeltaPercent } else { 'n/a' }
    $pd = if ($row.baselineSamples) { '{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.p95DeltaMs,$row.p95DeltaPercent } else { 'n/a' }
    $lines += '| {0} | {1} | {2:F4}/{3:F4} | {4} | {5} | {6}/{7} |' -f $row.metric,$before,$row.candidateMedianMs,$row.candidateP95Ms,$md,$pd,$row.baselineSamples,$row.candidateSamples
}
$lines += ''
$lines += 'Nearest-rank percentiles; p95 with 15 samples is the maximum. Frames within one launch are correlated; inspect launch means. Shared native launches alternate order; component harnesses run as separately warmed binary blocks with reversed order between tests. Candidate-only behavior has no baseline equivalent. Branch drawing in the component harness is a legacy adapter, not native frame latency. Loopback HTTP excludes employer-network latency, authentication configuration and display scanout. Retain all threshold crossings and validate stage completeness separately.'
$lines | Set-Content (Join-Path $OutputDirectory 'summary.md')
$rows | Format-Table metric,baselineMedianMs,candidateMedianMs,baselineP95Ms,candidateP95Ms,medianDeltaPercent
