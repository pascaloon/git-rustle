param(
    [Parameter(Mandatory)][string]$Run,
    [string[]]$Suites = @('source','small','plain')
)
$ErrorActionPreference = 'Stop'
$Run = (Resolve-Path -LiteralPath $Run).Path
function Percentile($values, [double]$p) {
    $values = @($values | Where-Object { $null -ne $_ } | Sort-Object)
    if (-not $values.Count) { return $null }
    $values[[Math]::Max(0, [Math]::Ceiling($values.Count * $p) - 1)]
}
function Metric($name, $before, $after) {
    $b = Percentile $before .5
    $c = Percentile $after .5
    [pscustomobject]@{
        metric=$name; baselineMedian=$b; candidateMedian=$c
        baselineP95=(Percentile $before .95); candidateP95=(Percentile $after .95)
        medianDelta=if ($null -ne $b -and $null -ne $c) {$c-$b} else {$null}
        medianDeltaPercent=if ($b -gt 0 -and $null -ne $c) {100*($c-$b)/$b} else {$null}
        baselineSamples=@($before | Where-Object {$null -ne $_}).Count
        candidateSamples=@($after | Where-Object {$null -ne $_}).Count
    }
}
$summary = @()
$startup = Get-Content (Join-Path $Run 'startup/samples.json') -Raw | ConvertFrom-Json
foreach ($mode in 'warm','uncached') {
    $b = @($startup | Where-Object {$_.binary -eq 'baseline' -and $_.mode -eq $mode})
    $c = @($startup | Where-Object {$_.binary -eq 'candidate' -and $_.mode -eq $mode})
    $summary += Metric "startup-$mode-ms" $b.firstFrameMs $c.firstFrameMs
    if ($mode -eq 'uncached') { $summary += Metric 'walk-uncached-ms' $b.walkDoneMs $c.walkDoneMs }
}
foreach ($suite in $Suites) {
    $samples = Get-Content (Join-Path $Run "$suite/samples.json") -Raw | ConvertFrom-Json
    foreach ($scenario in @($samples.scenario | Select-Object -Unique)) {
        $b = @($samples | Where-Object {$_.binary -eq 'baseline' -and $_.scenario -eq $scenario})
        $c = @($samples | Where-Object {$_.binary -eq 'candidate' -and $_.scenario -eq $scenario})
        if ($scenario -eq 'diff') {
            $summary += Metric "$suite-diff-ready-ms" $b.workerMs $c.workerMs
            $summary += Metric "$suite-scenario-frame-ms" $b.scenarioFrameAfterRequestMs $c.scenarioFrameAfterRequestMs
            $summary += Metric "$suite-syntax-ready-ms" @() $c.syntaxReadyMs
        } else {
            $summary += Metric "$suite-$scenario-frame-ms" $b.frameIntervalsMs $c.frameIntervalsMs
            $bm = @($b | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
            $cm = @($c | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
            $summary += Metric "$suite-$scenario-launch-mean-ms" $bm $cm
            if ($scenario -eq 'wrap-scroll') { $summary += Metric "$suite-wrap-layout-ms" $b.wrapLayoutMs $c.wrapLayoutMs }
        }
    }
}
$summary | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $Run 'summary.json')
$summary | Format-Table metric,baselineMedian,candidateMedian,baselineP95,candidateP95,medianDeltaPercent
