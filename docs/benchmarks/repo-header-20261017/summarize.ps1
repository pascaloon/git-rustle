$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/repo-header-run.txt
function Percentile($values, [double]$p) {
    $sorted = @($values | Sort-Object)
    $sorted[[Math]::Max(0,[Math]::Ceiling($sorted.Count*$p)-1)]
}
$summary = @()
$raw = @()
foreach ($suite in @('absolute','relative')) {
    $samples = Get-Content (Join-Path $benchRun "$suite/samples.json") -Raw | ConvertFrom-Json
    foreach ($sample in $samples) {
        $log = Get-Content (Join-Path $benchRun "$suite/$($sample.binary)-$($sample.mode)-$($sample.sample).txt") -Raw
        $mark = [regex]::Match($log,'([\d.]+) ms  repo open \(')
        if (-not $mark.Success) { throw 'Missing repository readiness mark' }
        $sample | Add-Member repositoryReadyMs ([double]::Parse($mark.Groups[1].Value,[cultureinfo]::InvariantCulture))
        $sample | Add-Member launchPath $suite
        $raw += $sample
    }
    foreach ($mode in @('warm','uncached')) {
        foreach ($metric in @('firstFrameMs','repositoryReadyMs','walkDoneMs')) {
            if ($metric -eq 'walkDoneMs' -and $mode -eq 'warm') { continue }
            $b = @($samples | Where-Object { $_.binary -eq 'baseline' -and $_.mode -eq $mode } | ForEach-Object { $_.$metric })
            $c = @($samples | Where-Object { $_.binary -eq 'candidate' -and $_.mode -eq $mode } | ForEach-Object { $_.$metric })
            if ($b.Count -ne 15 -or $c.Count -ne 15 -or $b.Contains($null) -or $c.Contains($null)) { throw 'Incomplete metrics' }
            $bm = Percentile $b .5; $cm = Percentile $c .5
            $bp = Percentile $b .95; $cp = Percentile $c .95
            $summary += [pscustomobject]@{launchPath=$suite;mode=$mode;metric=$metric;baselineMedian=$bm;candidateMedian=$cm;baselineP95=$bp;candidateP95=$cp;medianDelta=($cm-$bm);medianPercent=(100*($cm-$bm)/$bm);p95Delta=($cp-$bp);samples=15}
        }
    }
}
$raw | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $benchRun 'samples-with-repository-ready.json')
$summary | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $benchRun 'summary.json')
$summary | Format-Table launchPath,mode,metric,baselineMedian,candidateMedian,baselineP95,candidateP95,medianDelta,medianPercent
