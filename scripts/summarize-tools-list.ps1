param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference='Stop'
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
# Reuse the established raw/native/component parser and strict sample checks.
# It rebuilds the base table on every invocation, keeping this extension idempotent.
& "$PSScriptRoot/summarize-todos.ps1" -OutputDirectory $OutputDirectory
$rows=[Collections.Generic.List[object]]::new()
foreach($row in @(Get-Content -LiteralPath (Join-Path $OutputDirectory 'summary.json') -Raw|ConvertFrom-Json)){$rows.Add($row)}
function Pctl($Values,[double]$P){$sorted=@($Values|Sort-Object);return [double]$sorted[[Math]::Ceiling($sorted.Count*$P)-1]}
$actionPath=Join-Path $OutputDirectory 'actions/samples.json'
if(Test-Path -LiteralPath $actionPath){
    $samples=@(Get-Content -LiteralPath $actionPath -Raw|ConvertFrom-Json|Where-Object {$_.binary-eq'candidate'-and$_.scenario-eq'external-tools'})
    foreach($field in 'firstScanMs','configureMs','saveMs','selectionMs','removeMs','pickerMs'){
        $values=@(foreach($sample in $samples){
            $value=$sample.toolListMetrics.$field
            if($null-eq$value-or$value-isnot[ValueType]-or$value-is[bool]-or[double]::IsNaN($value)-or[double]::IsInfinity($value)-or$value-lt0){throw "Missing/invalid native tool-list $field"}
            [double]$value
        })
        if(!$values.Count){throw 'No native tools-list samples'}
        $rows.Add([pscustomobject]@{metric="external-tools-$field";kind='native-worker';comparisonKind='candidate-only';dimensions=$null;baselineSamples=0;candidateSamples=$values.Count
            baselineMedianMs=$null;candidateMedianMs=(Pctl $values .5);baselineP95Ms=$null;candidateP95Ms=(Pctl $values .95);medianDeltaMs=$null;p95DeltaMs=$null;medianDeltaPercent=$null;p95DeltaPercent=$null})
    }
}
$gates=[Collections.Generic.List[object]]::new()
foreach($row in $rows){
    $threshold=if($row.kind-in@('native-frame','native-launch-mean')){.10}elseif($row.kind-eq'native-worker'){1.0}elseif($row.kind-eq'component'){
        if($row.metric-cmatch'^external_tools-tests-benchmark_external_tools-(detect_ms|prepare_ms|launch_wait_ms|merge_ms)$'){1.0}
        elseif($row.metric-cmatch'^external_tools-tests-benchmark_tool_list-selection_1000_ms$'){.10}
        elseif($row.metric-cmatch'^external_tools-tests-benchmark_tool_list-(first_scan_ms|repeat_load_ms|save_ms|migrate_ms)$'){1.0}
        else{throw "Unclassified tools-list component: $($row.metric)"}
    }else{throw "Unclassified tools-list metric kind: $($row.kind)"}
    $shared=$row.comparisonKind-eq'baseline-candidate'
    $medianFlag=$shared-and$null-ne$row.medianDeltaPercent-and($row.medianDeltaMs-$threshold)-gt1e-9-and($row.medianDeltaPercent-5)-gt1e-9
    $p95Flag=$shared-and$null-ne$row.p95DeltaPercent-and($row.p95DeltaMs-$threshold)-gt1e-9-and($row.p95DeltaPercent-5)-gt1e-9
    $gates.Add([pscustomobject]@{metric=$row.metric;comparisonKind=$row.comparisonKind;absoluteThresholdMs=$threshold;percentThreshold=5;medianCrossing=$medianFlag;p95Crossing=$p95Flag
        status=if(!$shared){'candidate-only'}elseif($null-eq$row.medianDeltaPercent-or$null-eq$row.p95DeltaPercent){'not-evaluable-zero-baseline'}elseif($medianFlag-or$p95Flag){'investigate-joint-threshold'}else{'below-joint-threshold'}
        baselineMedianMs=$row.baselineMedianMs;candidateMedianMs=$row.candidateMedianMs;baselineP95Ms=$row.baselineP95Ms;candidateP95Ms=$row.candidateP95Ms
        medianDeltaMs=$row.medianDeltaMs;medianDeltaPercent=$row.medianDeltaPercent;p95DeltaMs=$row.p95DeltaMs;p95DeltaPercent=$row.p95DeltaPercent})
}
ConvertTo-Json -InputObject @($rows) -Depth 7|Set-Content -LiteralPath (Join-Path $OutputDirectory 'summary.json')
ConvertTo-Json -InputObject @($gates) -Depth 7|Set-Content -LiteralPath (Join-Path $OutputDirectory 'gates-metrics.json')
$crossings=@($gates|Where-Object {$_.medianCrossing-or$_.p95Crossing})
ConvertTo-Json -InputObject @($crossings) -Depth 7|Set-Content -LiteralPath (Join-Path $OutputDirectory 'gates-crossings.json')
$lines=@('| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta ms / % | p95 delta ms / % | Status |','| --- | ---: | ---: | ---: | ---: | --- |')
for($i=0;$i-lt$rows.Count;$i++){
    $row=$rows[$i];$gate=$gates[$i]
    $before=if($row.baselineSamples){'{0:F4}/{1:F4}'-f$row.baselineMedianMs,$row.baselineP95Ms}else{'candidate-only'}
    $md=if($null-ne$row.medianDeltaPercent){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%'-f$row.medianDeltaMs,$row.medianDeltaPercent}else{'n/a'}
    $pd=if($null-ne$row.p95DeltaPercent){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%'-f$row.p95DeltaMs,$row.p95DeltaPercent}else{'n/a'}
    $lines+='| {0} | {1} | {2:F4}/{3:F4} | {4} | {5} | {6} |'-f$row.metric,$before,$row.candidateMedianMs,$row.candidateP95Ms,$md,$pd,$gate.status
}
$lines+='';$lines+='Joint gates use strict >5% and >1ms for startup/worker intervals, or >5% and >0.10ms for native frames/launch means and stable-ID lookup CPU. Tiny1e-9 boundary tolerance prevents floating-point equality flags. Candidate-only costs have no baseline equivalent. Frame samples within one launch are correlated; launch means stay separate. Native list intervals include action through verified painted state; firstScanMs is startup config-worker request through delivery. Fixture preparation/checks are outside clocks. Retain every crossing and bounded repeat; this table is not automatic performance acceptance or a zero-regression guarantee.'
$lines|Set-Content -LiteralPath (Join-Path $OutputDirectory 'summary.md')
Write-Output "$($rows.Count) tools-list metrics; $($crossings.Count) shared joint-threshold crossings"
