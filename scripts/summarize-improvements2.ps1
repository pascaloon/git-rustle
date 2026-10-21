param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference='Stop'
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$rows=[Collections.Generic.List[object]]::new();$stages=[Collections.Generic.List[object]]::new()
function Pctl($Values,[double]$P){$sorted=@($Values|Sort-Object);if(!$sorted.Count){throw 'Missing timing samples'};[double]$sorted[[Math]::Ceiling($sorted.Count*$P)-1]}
function Metric([string]$Name,$Before,$After,[string]$Kind,$Dimensions){
    $b=@($Before|Where-Object{$null-ne$_});$c=@($After|Where-Object{$null-ne$_})
    if(!$c.Count){throw "No candidate values: $Name"}
    foreach($v in @($b)+@($c)){if($v-isnot[ValueType]-or$v-is[bool]-or[double]::IsNaN([double]$v)-or[double]::IsInfinity([double]$v)-or$v-lt0){throw "Invalid timing: $Name"}}
    $bm=if($b.Count){Pctl $b .5}else{$null};$bp=if($b.Count){Pctl $b .95}else{$null};$cm=Pctl $c .5;$cp=Pctl $c .95
    [pscustomobject]@{metric=$Name;kind=$Kind;dimensions=$Dimensions;comparisonKind=if($b.Count){'baseline-candidate'}else{'candidate-only'};baselineSamples=$b.Count;candidateSamples=$c.Count;baselineMedianMs=$bm;candidateMedianMs=$cm;baselineP95Ms=$bp;candidateP95Ms=$cp;medianDeltaMs=if($b.Count){$cm-$bm}else{$null};p95DeltaMs=if($b.Count){$cp-$bp}else{$null};medianDeltaPercent=if($b.Count-and$bm-gt0){100*($cm-$bm)/$bm}else{$null};p95DeltaPercent=if($b.Count-and$bp-gt0){100*($cp-$bp)/$bp}else{$null}}
}
function Check-Group($Group,[int]$Expected,[string]$Name){
    if($Group.Count-ne$Expected){throw "Expected $Expected samples for $Name; got $($Group.Count)"}
    $ids=@($Group.sample|Sort-Object -Unique)
    if($ids.Count-ne$Expected-or$ids[0]-lt0-or$ids[-1]-$ids[0]-ne$Expected-1){throw "Duplicate or noncontiguous samples: $Name"}
}
function Native([string]$Relative,[string]$Prefix,[bool]$Shared,[string[]]$ExpectedScenarios){
    $directory=Join-Path $OutputDirectory $Relative
    if(!(Test-Path -LiteralPath $directory)){return}
    $path=Join-Path $directory 'samples.json';if(!(Test-Path -LiteralPath $path)){throw "Incomplete native stage $Relative"}
    $samples=@(Get-Content -LiteralPath $path -Raw|ConvertFrom-Json)
    $configPath=Join-Path $directory 'stage-config.json'
    $expected=if(Test-Path -LiteralPath $configPath){[int](Get-Content -LiteralPath $configPath -Raw|ConvertFrom-Json).samples}else{
        $provenance=@(Get-ChildItem -LiteralPath $OutputDirectory -Filter 'provenance-*.json'|Sort-Object LastWriteTime)
        if(!$provenance.Count){throw 'Missing provenance for expected sample count'}
        [int](Get-Content -LiteralPath $provenance[-1].FullName -Raw|ConvertFrom-Json).samples
    }
    if($samples.Count-ne$expected*$ExpectedScenarios.Count*$(if($Shared){2}else{1})){throw "Unexpected native group count in $Relative"}
    foreach($scenario in $ExpectedScenarios){
        $b=@($samples|Where-Object{$_.binary-eq'baseline'-and$(if($Relative-eq'startup'){$_.mode}else{$_.scenario})-ceq$scenario})
        $c=@($samples|Where-Object{$_.binary-eq'candidate'-and$(if($Relative-eq'startup'){$_.mode}else{$_.scenario})-ceq$scenario})
        Check-Group $c $expected "$Relative/$scenario/candidate";if($Shared){Check-Group $b $expected "$Relative/$scenario/baseline"}
        if($Relative-eq'startup'){
            $rows.Add((Metric "startup-$scenario-first-frame" $b.firstFrameMs $c.firstFrameMs 'worker' $null))
            if($scenario-eq'uncached'){
                if(@($b+$c|Where-Object{$null-eq$_.walkDoneMs}).Count){throw 'Missing uncached completed walk'}
                $rows.Add((Metric "startup-$scenario-complete-walk" $b.walkDoneMs $c.walkDoneMs 'worker' $null))
            }
        }else{
            $bf=@($b|ForEach-Object frameIntervalsMs|Where-Object{$null-ne$_});$cf=@($c|ForEach-Object frameIntervalsMs|Where-Object{$null-ne$_})
            if($cf.Count){
                foreach($launch in @($b)+@($c)){if(@($launch.frameIntervalsMs).Count-ne120){throw "Expected120 frames: $Relative/$scenario"}}
                $rows.Add((Metric "$Prefix$scenario-frames" $bf $cf 'frame' $null))
                $bm=@($b|ForEach-Object{($_.frameIntervalsMs|Measure-Object -Average).Average});$cm=@($c|ForEach-Object{($_.frameIntervalsMs|Measure-Object -Average).Average})
                $rows.Add((Metric "$Prefix$scenario-launch-means" $bm $cm 'frame' $null))
            }elseif($scenario-in'scroll','search-scroll','graph-hover'){throw "Missing frame observations: $scenario"}
            if(@($c.workerMs|Where-Object{$null-ne$_}).Count){$rows.Add((Metric "$Prefix$scenario-request-to-frame" $b.workerMs $c.workerMs 'worker' $null))}
            $bh=@($b|ForEach-Object todoHandlerMs|Where-Object{$null-ne$_});$ch=@($c|ForEach-Object todoHandlerMs|Where-Object{$null-ne$_})
            if($ch.Count){
                foreach($launch in @($b)+@($c)){if(@($launch.todoHandlerMs).Count-ne120){throw "Expected120 handler samples: $scenario"}}
                $rows.Add((Metric "$Prefix$scenario-handler-cpu" $bh $ch 'cpu' $null))
                $bm=@($b|ForEach-Object{($_.todoHandlerMs|Measure-Object -Average).Average});$cm=@($c|ForEach-Object{($_.todoHandlerMs|Measure-Object -Average).Average})
                $rows.Add((Metric "$Prefix$scenario-handler-launch-means" $bm $cm 'cpu' $null))
            }
        }
    }
    $stages.Add([pscustomobject]@{stage=$Relative;retainedLaunches=$samples.Count;expectedPerGroup=$expected})
}
if(Test-Path -LiteralPath (Join-Path $OutputDirectory 'actions')){
    foreach($relative in 'actions/stashes/shared/samples.json','actions/stashes/create/samples.json','actions/stashes/create-intervals.json','actions/stashes/baseline-stashes.txt','actions/stashes/candidate-stashes.txt','actions/stashes/candidate-create.txt','actions/discard/samples.json'){
        if(!(Test-Path -LiteralPath (Join-Path $OutputDirectory $relative))){throw "Incomplete action stage: $relative"}
    }
}
Native 'startup' '' $true @('warm','uncached')
Native 'history' '' $true @('scroll','search-scroll','graph-hover','status-panels')
Native 'refresh' '' $false @('refresh-feedback')
Native 'actions/stashes/shared' '' $true @('stash-panel','stash-apply','stash-delete')
Native 'actions/stashes/create' '' $false @('stash-create')
$createPath=Join-Path $OutputDirectory 'actions/stashes/create-intervals.json'
if(Test-Path -LiteralPath $createPath){
    $intervals=@(Get-Content -LiteralPath $createPath -Raw|ConvertFrom-Json)
    $operations=@('create','external add','external drop')
    if(@($intervals|Where-Object operation -CEQ 'global refresh').Count){$operations+='global refresh'}
    foreach($op in $operations){
        $group=@($intervals|Where-Object operation -CEQ $op);$count=@($intervals|Where-Object operation -CEQ 'create').Count
        Check-Group $group $count "stash/$op";$rows.Add((Metric "stash-$op-paint" @() $group.ms 'worker' $null))
    }
}
$discardPath=Join-Path $OutputDirectory 'actions/discard/samples.json'
if(Test-Path -LiteralPath $discardPath){
    $all=@(Get-Content -LiteralPath $discardPath -Raw|ConvertFrom-Json);$values=@($all|Where-Object{!$_.warmup})
    if(@($all|Where-Object warmup).Count-ne1){throw 'Expected one discarded native discard warmup'}
    $config=Get-Content -LiteralPath (Join-Path $OutputDirectory 'actions/stage-config.json') -Raw|ConvertFrom-Json
    Check-Group $values ([int]$config.samples) 'pending-discard'
    foreach($field in 'menuMs','armedMs','discardWorkerMs','discardFrameMs'){$rows.Add((Metric "pending-discard-$field" @() $values.$field 'worker' $null))}
    $stages.Add([pscustomobject]@{stage='actions/discard';retainedLaunches=$values.Count;expectedPerGroup=$config.samples})
}
function Read-Measures([string]$Path){
    $text=Get-Content -LiteralPath $Path -Raw
    if($text-notmatch'running 1 test'-or$text-notmatch'test result: ok\. 1 passed;'){throw "Unsuccessful harness: $Path"}
    $records=[Collections.Generic.List[object]]::new()
    foreach($m in [regex]::Matches($text,'(?m)^\{[^\r\n]*\}\r?$')){$records.Add(($m.Value|ConvertFrom-Json))}
    foreach($m in [regex]::Matches($text,'(?m)^[^\r\n]*samples:\s*(\[[^\r\n]*\])')){foreach($r in @($m.Groups[1].Value|ConvertFrom-Json)){$records.Add($r)}}
    foreach($m in [regex]::Matches($text,'(?m)^discard_file sample=(\d+) warmup=(true|false) worker_ms=([\d.]+)')){
        if($m.Groups[2].Value-ceq'false'){$records.Add([pscustomobject]@{sample=[int]$m.Groups[1].Value;worker_ms=[double]::Parse($m.Groups[3].Value,[cultureinfo]::InvariantCulture)})}
    }
    $map=[Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
    foreach($record in $records){
        if(!($record.PSObject.Properties.Name-contains'sample')-or$record.sample-lt0){continue}
        $dimensions=[ordered]@{}
        foreach($p in $record.PSObject.Properties){if($p.Name-ne'sample'-and$p.Name-cnotmatch'(?i)ms$'-and$p.Name-ne'warmup'){$dimensions[$p.Name]=$p.Value}}
        $prefix=(@($dimensions.Keys|ForEach-Object{"$_=$([Uri]::EscapeDataString([string]$dimensions[$_]))"})-join',')
        foreach($p in $record.PSObject.Properties|Where-Object Name -Match '(?i)ms$'){
            $key=if($prefix){"$prefix-$($p.Name)"}else{$p.Name}
            if(!$map.ContainsKey($key)){$map.Add($key,[pscustomobject]@{dimensions=$dimensions;values=[Collections.Generic.List[double]]::new();ids=[Collections.Generic.List[int]]::new()})}
            if($null-eq$p.Value-or$p.Value-is[bool]-or$p.Value-isnot[ValueType]){throw "Invalid component value: $key"}
            $map[$key].values.Add([double]$p.Value);$map[$key].ids.Add([int]$record.sample)
        }
    }
    if(!$map.Count){throw "No recognized measurements: $Path"}
    foreach($key in $map.Keys){$g=$map[$key];$ids=@($g.ids|Sort-Object -Unique);if($g.values.Count-ne15-or$ids.Count-ne15-or$ids[-1]-$ids[0]-ne14){throw "Expected15 unique retained component samples for $key ($Path)"}}
    return ,$map
}
function Components([string]$Directory,[string]$Prefix){
    if(!(Test-Path -LiteralPath $Directory)){return}
    foreach($file in Get-ChildItem -LiteralPath $Directory -Filter 'candidate-*.txt'){
        $name=$file.BaseName.Substring(10);$c=Read-Measures $file.FullName
        $baselinePath=Join-Path $Directory ('baseline-'+$file.Name.Substring(10));$b=if(Test-Path -LiteralPath $baselinePath){Read-Measures $baselinePath}else{[Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)}
        foreach($key in $b.Keys){if(!$c.ContainsKey($key)){throw "Candidate omitted shared group $name/$key"}}
        foreach($key in $c.Keys|Sort-Object){$before=if($b.ContainsKey($key)){@($b[$key].values)}else{@()};$kind=if($name-match'hover'-or($name-eq'git-worktree-tests-benchmark_pending'-and$key-match'(bulkCheckMs|selectDrawMs)$')){'cpu'}else{'worker'};$rows.Add((Metric "$Prefix$name-$key" $before @($c[$key].values) $kind $c[$key].dimensions))}
    }
}
$componentDirectory=Join-Path $OutputDirectory 'components'
if(Test-Path -LiteralPath $componentDirectory){
    $config=Get-Content -LiteralPath (Join-Path $componentDirectory 'harness-config.json') -Raw|ConvertFrom-Json
    foreach($test in @($config.shared)+@($config.candidateOnly)){foreach($role in $(if($test-in$config.shared){@('baseline','candidate')}else{@('candidate')})){
        if(!(Test-Path -LiteralPath (Join-Path $componentDirectory "$role-$($test-replace'::','-').txt"))){throw "Missing component $role/$test"}
    }}
    Components $componentDirectory ''
}
Components (Join-Path $OutputDirectory 'actions/stashes') 'stash-worker-'
$gates=[Collections.Generic.List[object]]::new()
foreach($row in $rows){
    $threshold=if($row.kind-in'cpu','frame'){.10}else{1.0};$shared=$row.comparisonKind-eq'baseline-candidate'
    $median=$shared-and$null-ne$row.medianDeltaPercent-and$row.medianDeltaMs-$threshold-gt1e-9-and$row.medianDeltaPercent-5-gt1e-9
    $p95=$shared-and$null-ne$row.p95DeltaPercent-and$row.p95DeltaMs-$threshold-gt1e-9-and$row.p95DeltaPercent-5-gt1e-9
    $gates.Add([pscustomobject]@{metric=$row.metric;absoluteThresholdMs=$threshold;percentThreshold=5;medianCrossing=$median;p95Crossing=$p95;status=if(!$shared){'candidate-only'}elseif($null-eq$row.medianDeltaPercent-or$null-eq$row.p95DeltaPercent){'not-evaluable-zero-baseline'}elseif($median-or$p95){'investigate-joint-threshold'}else{'below-joint-threshold'}})
}
$crossings=@($gates|Where-Object{$_.medianCrossing-or$_.p95Crossing})
ConvertTo-Json -InputObject @($rows) -Depth 8|Set-Content (Join-Path $OutputDirectory 'summary.json')
ConvertTo-Json -InputObject @($gates) -Depth 6|Set-Content (Join-Path $OutputDirectory 'gates-metrics.json')
ConvertTo-Json -InputObject @($crossings) -Depth 6|Set-Content (Join-Path $OutputDirectory 'gates-crossings.json')
ConvertTo-Json -InputObject @($stages) -Depth 6|Set-Content (Join-Path $OutputDirectory 'stage-completeness.json')
$lines=@('# Improvements measurement report','','| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta ms / % | p95 delta ms / % | B/C samples | Status |','|---|---:|---:|---:|---:|---:|---|')
for($i=0;$i-lt$rows.Count;$i++){
    $r=$rows[$i];$before=if($r.baselineSamples){'{0:F4}/{1:F4}'-f$r.baselineMedianMs,$r.baselineP95Ms}else{'candidate-only'}
    $md=if($null-ne$r.medianDeltaPercent){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%'-f$r.medianDeltaMs,$r.medianDeltaPercent}else{'n/a'}
    $pd=if($null-ne$r.p95DeltaPercent){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%'-f$r.p95DeltaMs,$r.p95DeltaPercent}else{'n/a'}
    $lines+='| {0} | {1} | {2:F4}/{3:F4} | {4} | {5} | {6}/{7} | {8} |'-f$r.metric,$before,$r.candidateMedianMs,$r.candidateP95Ms,$md,$pd,$r.baselineSamples,$r.candidateSamples,$gates[$i].status
}
$lines+=@('','Strict joint gates require >5% and >1ms for startup, worker I/O and native request-to-paint intervals; native frames, launch means and in-memory CPU use >5% and >0.10ms. Nearest-rank p95 with fifteen retained samples is the maximum. Pooled frames within a launch are correlated, so launch means remain separate. Candidate-only features establish absolute costs without an invented baseline comparison. Source/binary/fixture integrity is recorded per invocation, and stage-completeness.json records observed groups.','', 'Shared native order alternates. Worker binaries run sequentially in separate blocks, reversing order between shared component tests; each component includes its own warmup and fifteen retained samples. Tagged dimensions stay separate. Selected-path CPU uses 100,000 history rows and an 80-row viewport; it does not establish every graph shape. Stash/native discard use tiny marked local fixtures. Native discardWorkerMs isolates dispatch through worker result delivery, including recovery checkpoints; discardFrameMs also includes pending refresh and post-result verification reads/Git subprocesses before the verified-frame mark. External stash intervals include background mutation and normal two-second status scheduling. Startup uses separate cache roots because baseline and candidate cache formats differ. The cache warm-up for each binary is independent. No public employer network, real editor, physical pointer input or display scanout is measured. Shared pending status includes the added bounded stash metadata probes. Preserve all crossings and any diagnostic repeats; finite measurements cannot guarantee zero regression or clear earlier unresolved measurements outside this bundle.')
$lines|Set-Content (Join-Path $OutputDirectory 'report.md')
$lines|Set-Content (Join-Path $OutputDirectory 'summary.md')
Write-Output "$($rows.Count) metrics; $($crossings.Count) shared joint-threshold crossings"
