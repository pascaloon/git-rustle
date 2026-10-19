param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
function Percentile($Values,[double]$p) {
    $sorted = @($Values | Sort-Object)
    if (!$sorted.Count) { throw 'Cannot summarize missing timing samples' }
    return $sorted[[Math]::Max(0,[Math]::Ceiling($sorted.Count * $p)-1)]
}
function Metric([string]$Name,$Before,$After) {
    $b = @($Before | Where-Object { $null -ne $_ })
    $c = @($After | Where-Object { $null -ne $_ })
    if (!$c.Count) { throw "Missing candidate samples for $Name" }
    $median = Percentile $c .5
    $baselineMedian = if ($b.Count) { Percentile $b .5 } else { $null }
    return [pscustomobject]@{
        metric = $Name; baselineSamples = $b.Count; candidateSamples = $c.Count
        baselineMedianMs = $baselineMedian; candidateMedianMs = $median
        baselineP95Ms = if ($b.Count) { Percentile $b .95 } else { $null }
        candidateP95Ms = Percentile $c .95
        medianDeltaMs = if ($b.Count) { $median-$baselineMedian } else { $null }
        medianDeltaPercent = if ($b.Count -and $baselineMedian -gt 0) { 100*($median-$baselineMedian)/$baselineMedian } else { $null }
        p95DeltaPercent = if ($b.Count -and (Percentile $b .95) -gt 0) { 100*((Percentile $c .95)-(Percentile $b .95))/(Percentile $b .95) } else { $null }
    }
}
$rows = @()
$startup = Join-Path $OutputDirectory 'startup/samples.json'
if (Test-Path -LiteralPath $startup) {
    $samples = Get-Content -LiteralPath $startup -Raw | ConvertFrom-Json
    foreach ($mode in @('warm','uncached')) {
        $b = @($samples | Where-Object { $_.binary -eq 'baseline' -and $_.mode -eq $mode })
        $c = @($samples | Where-Object { $_.binary -eq 'candidate' -and $_.mode -eq $mode })
        $rows += Metric "startup-$mode-first-frame" $b.firstFrameMs $c.firstFrameMs
        if (@($c.walkDoneMs | Where-Object { $null -ne $_ }).Count) {
            $rows += Metric "startup-$mode-complete-walk" $b.walkDoneMs $c.walkDoneMs
        }
    }
}
foreach ($suite in @('history','actions')) {
    $path = Join-Path $OutputDirectory "$suite/samples.json"
    if (!(Test-Path -LiteralPath $path)) { continue }
    $samples = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    foreach ($scenario in @($samples.scenario | Select-Object -Unique)) {
        $b = @($samples | Where-Object { $_.binary -eq 'baseline' -and $_.scenario -eq $scenario })
        $c = @($samples | Where-Object { $_.binary -eq 'candidate' -and $_.scenario -eq $scenario })
        $bf = @($b | ForEach-Object { $_.frameIntervalsMs } | Where-Object { $null -ne $_ })
        $cf = @($c | ForEach-Object { $_.frameIntervalsMs } | Where-Object { $null -ne $_ })
        if ($cf.Count) {
            $rows += Metric "$scenario-frames" $bf $cf
            $bm = @($b | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
            $cm = @($c | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
            $rows += Metric "$scenario-launch-means" $bm $cm
        }
        if (@($c.workerMs | Where-Object { $null -ne $_ }).Count) {
            $rows += Metric "$scenario-request-to-delivery" $b.workerMs $c.workerMs
        }
    }
}
$preview = Join-Path $OutputDirectory 'preview/samples.json'
if (Test-Path -LiteralPath $preview) {
    $samples = Get-Content -LiteralPath $preview -Raw | ConvertFrom-Json
    $b = @($samples | Where-Object binary -eq 'baseline')
    $c = @($samples | Where-Object binary -eq 'candidate')
    $rows += Metric 'remote-preview-worker' $b.previewWorkerMs $c.previewWorkerMs
    $rows += Metric 'remote-preview-since-startup' $b.previewReadyMs $c.previewReadyMs
}
$providerFixture = Join-Path $OutputDirectory 'components/provider-fixture/samples.json'
if (Test-Path -LiteralPath $providerFixture) {
    $samples = Get-Content -LiteralPath $providerFixture -Raw | ConvertFrom-Json
    $rows += Metric 'provider-loopback-complete-fixture-process' @() $samples.elapsedMs
}
$rows | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory 'summary.json')
$lines = @('| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta | p95 delta | Samples B/C |',
    '| --- | ---: | ---: | ---: | ---: | ---: |')
foreach ($row in $rows) {
    $before = if ($row.baselineSamples) { '{0:F3}/{1:F3}' -f $row.baselineMedianMs,$row.baselineP95Ms } else { 'candidate only' }
    $delta = if ($row.baselineSamples) { '{0:+0.00;-0.00;0.00}%' -f $row.medianDeltaPercent } else { 'n/a' }
    $p95 = if ($row.baselineSamples) { '{0:+0.00;-0.00;0.00}%' -f $row.p95DeltaPercent } else { 'n/a' }
    $lines += '| {0} | {1} | {2:F3}/{3:F3} | {4} | {5} | {6}/{7} |' -f $row.metric,$before,$row.candidateMedianMs,$row.candidateP95Ms,$delta,$p95,$row.baselineSamples,$row.candidateSamples
}
$lines += ''
$lines += 'Nearest-rank percentiles; frame samples within a launch are correlated. Compare launch means as well as pooled frames. Missing stages are omitted, so this table alone does not establish suite completion. Component harness output remains under components/. Candidate-only scenarios have no equivalent in the preserved baseline. The complete provider fixture process includes process launch, temporary fixture construction, both providers, paging/project/offline-cache assertions and redirected logging; it is not isolated HTTP or native UI delivery latency.'
$lines | Set-Content (Join-Path $OutputDirectory 'summary.md')
$rows | Format-Table metric,baselineMedianMs,candidateMedianMs,baselineP95Ms,candidateP95Ms,medianDeltaPercent
