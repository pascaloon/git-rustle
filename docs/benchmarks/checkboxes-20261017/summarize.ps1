$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/checkboxes-run.txt
function Percentile($values, [double]$p) {
    $sorted = @($values | Sort-Object)
    return $sorted[[Math]::Max(0, [Math]::Ceiling($sorted.Count * $p) - 1)]
}
function Compare-Metric($name, $before, $after) {
    $b = Percentile $before .5
    $c = Percentile $after .5
    return [pscustomobject]@{metric=$name;baselineMedian=$b;candidateMedian=$c;baselineP95=(Percentile $before .95);candidateP95=(Percentile $after .95);medianDelta=($c-$b);medianDeltaPercent=(100*($c-$b)/$b);baselineSamples=@($before).Count;candidateSamples=@($after).Count}
}
$startup = Get-Content (Join-Path $benchRun 'startup/samples.json') -Raw | ConvertFrom-Json
$summary = @()
foreach ($mode in @('warm','uncached')) {
    $b = @($startup | Where-Object { $_.binary -eq 'baseline' -and $_.mode -eq $mode })
    $c = @($startup | Where-Object { $_.binary -eq 'candidate' -and $_.mode -eq $mode })
    $summary += Compare-Metric "startup-$mode" $b.firstFrameMs $c.firstFrameMs
    if ($mode -eq 'uncached') { $summary += Compare-Metric 'full-walk-uncached' $b.walkDoneMs $c.walkDoneMs }
}
foreach ($suite in @('pending','diff')) {
    $samples = Get-Content (Join-Path $benchRun "$suite/samples.json") -Raw | ConvertFrom-Json
    foreach ($scenario in @($samples.scenario | Select-Object -Unique)) {
        $b = @($samples | Where-Object { $_.binary -eq 'baseline' -and $_.scenario -eq $scenario })
        $c = @($samples | Where-Object { $_.binary -eq 'candidate' -and $_.scenario -eq $scenario })
        $summary += Compare-Metric "$scenario-frames" $b.frameIntervalsMs $c.frameIntervalsMs
        $bm = @($b | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
        $cm = @($c | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
        $summary += Compare-Metric "$scenario-launch-means" $bm $cm
    }
}
$summary | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $benchRun 'summary.json')
$summary | Format-Table metric,baselineMedian,candidateMedian,baselineP95,candidateP95,medianDeltaPercent

