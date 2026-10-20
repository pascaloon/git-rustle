param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference='Stop'
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
# Reuse the established raw/native/component parser and strict sample checks.
# It rebuilds the base table on every invocation, keeping this extension idempotent.
$sourceRoot=(Resolve-Path "$PSScriptRoot/..").Path
& (Join-Path $sourceRoot 'scripts/summarize-todos.ps1') -OutputDirectory $OutputDirectory
$rows=[Collections.Generic.List[object]]::new()
foreach($row in @(Get-Content -LiteralPath (Join-Path $OutputDirectory 'summary.json') -Raw|ConvertFrom-Json)){$rows.Add($row)}
function Pctl($Values,[double]$P){$sorted=@($Values|Sort-Object);return [double]$sorted[[Math]::Ceiling($sorted.Count*$P)-1]}
$actionPath=Join-Path $OutputDirectory 'actions/samples.json'
if(Test-Path -LiteralPath $actionPath){
    $samples=@(Get-Content -LiteralPath $actionPath -Raw|ConvertFrom-Json)
    foreach($scenario in 'conflicts-merge','conflicts-rebase','conflicts-stash','conflicts-abort'){
        $group=@($samples|Where-Object {$_.binary-eq'candidate'-and$_.scenario-ceq$scenario})
        $fields=if($scenario-ceq'conflicts-abort'){@('openMs','abortMs')}else{@('openMs','incomingMs','localMs','wholeMs','fileMs','staleMs','rejectMs','reloadMs','saveMs','externalMs','continueMs')}
        foreach($field in $fields){
            $values=@(foreach($sample in $group){
                $value=$sample.conflictMetrics.$field
                if($null-eq$value-or$value-isnot[ValueType]-or$value-is[bool]-or[double]::IsNaN($value)-or[double]::IsInfinity($value)-or$value-lt0){throw "Missing/invalid native conflict $scenario/$field"}
                [double]$value
            })
            if(!$values.Count){throw "No conflict native samples: $scenario"}
            $rows.Add([pscustomobject]@{metric="$scenario-$field";kind='native-worker';comparisonKind='candidate-only';dimensions=$null;baselineSamples=0;candidateSamples=$values.Count
                baselineMedianMs=$null;candidateMedianMs=(Pctl $values .5);baselineP95Ms=$null;candidateP95Ms=(Pctl $values .95);medianDeltaMs=$null;p95DeltaMs=$null;medianDeltaPercent=$null;p95DeltaPercent=$null})
        }
    }
}
$gates=[Collections.Generic.List[object]]::new()
foreach($row in $rows){
    $threshold=if($row.kind-eq'native-worker'){
        if($row.metric-cnotmatch'^(startup-(warm|uncached)-(first-frame|complete-walk)|conflicts-(merge|rebase|stash|abort)-(request-to-frame|openMs|incomingMs|localMs|wholeMs|fileMs|staleMs|rejectMs|reloadMs|saveMs|externalMs|continueMs|abortMs))$'){throw "Unknown native conflict metric: $($row.metric)"}
        1.0
    }elseif($row.kind-in'native-frame','native-launch-mean'){
        if($row.metric-cnotmatch'^(scroll|search-scroll)-(frames|launch-means)$'){throw "Unknown shared rendering metric: $($row.metric)"}
        0.10
    }elseif($row.kind-eq'component'){
        if($row.metric-cmatch'^git-operations-tests-benchmark_stashes-(apply_ms|undo_ms|redo_ms|drop_ms|conflict_ms|abort_ms)$'){1.0}
        elseif($row.metric-cmatch'^git-operations-tests-benchmark_conflict_refresh-(metadata_ms|idle_20_ms|changed_ms|preview_ms)$'){1.0}
        else{throw "Unclassified conflict component: $($row.metric)"}
    }else{throw "Unclassified conflict metric kind: $($row.kind)"}
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
$lines+='';$lines+='Joint gates use strict >5% plus >1ms for startup, file-I/O workers and native action delivery; shared scroll frames and launch means use >5% plus >0.10ms. Tiny1e-9 tolerance prevents floating-point equality flags. Candidate-only costs have no valid baseline UI equivalent. Pooled frames within a launch are correlated; launch means remain separate. Conflict native intervals include actual production actions/confirmed resolution, watcher delivery after real external edit/index staging, and verified painted controls. Component idle_20_ms is 20 actual unchanged worker probes, not one probe and not an artificial delay. Metadata/changed/preview costs include bounded real repository I/O. Fixtures and postchecks are outside clocks. Preserve every crossing; finite measurements do not prove zero regression or clear older General/Search limitations.'
$lines|Set-Content -LiteralPath (Join-Path $OutputDirectory 'summary.md')
Write-Output "$($rows.Count) conflict metrics; $($crossings.Count) shared joint-threshold crossings"
