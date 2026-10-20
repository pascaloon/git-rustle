param([Parameter(Mandatory)][string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$rows = [Collections.Generic.List[object]]::new()
$stages = [Collections.Generic.List[object]]::new()
function Percentile($Values,[double]$P) {
    $sorted = @($Values | Sort-Object)
    if (!$sorted.Count) { throw 'Missing timing measurements' }
    return $sorted[[Math]::Ceiling($sorted.Count*$P)-1]
}
function Metric([string]$Name,$Before,$After,[string]$Kind,$Dimensions) {
    $b = @($Before | Where-Object { $null -ne $_ }); $c = @($After | Where-Object { $null -ne $_ })
    if (!$c.Count) { throw "Missing candidate samples: $Name" }
    foreach ($value in @($b)+@($c)) {
        if ($value -isnot [ValueType] -or $value -is [bool] -or [double]::IsNaN([double]$value) -or [double]::IsInfinity([double]$value) -or $value -lt 0) { throw "Invalid timing in $Name" }
    }
    $bm = if ($b.Count) { Percentile $b .5 } else { $null }; $bp = if ($b.Count) { Percentile $b .95 } else { $null }
    $cm = Percentile $c .5; $cp = Percentile $c .95
    return [pscustomobject]@{
        metric=$Name;kind=$Kind;comparisonKind=if($b.Count){'baseline-candidate'}else{'candidate-only'};dimensions=$Dimensions
        baselineSamples=$b.Count;candidateSamples=$c.Count;baselineMedianMs=$bm;candidateMedianMs=$cm;baselineP95Ms=$bp;candidateP95Ms=$cp
        medianDeltaMs=if($b.Count){$cm-$bm}else{$null};p95DeltaMs=if($b.Count){$cp-$bp}else{$null}
        medianDeltaPercent=if($b.Count -and $bm -gt 0){100*($cm-$bm)/$bm}else{$null}
        p95DeltaPercent=if($b.Count -and $bp -gt 0){100*($cp-$bp)/$bp}else{$null}
    }
}
function Check-NativeGroup($Group,[int]$Expected,[string]$Name) {
    if ($Expected -gt 0 -and $Group.Count -ne $Expected) { throw "Expected $Expected retained launches for $Name, got $($Group.Count)" }
    $ids = @($Group.sample | Sort-Object -Unique)
    if ($ids.Count -ne $Group.Count -or @($ids | Where-Object { $_ -lt 0 }).Count) { throw "Duplicate/warm-up native samples: $Name" }
    if($Expected -gt 0 -and ($ids[0] -ne 0 -or $ids[-1] -ne $Expected-1)){throw "Noncontiguous native sample indices: $Name"}
}
foreach ($stage in 'startup','history','actions') {
    $directory = Join-Path $OutputDirectory $stage; $path = Join-Path $directory 'samples.json'
    if (!(Test-Path -LiteralPath $directory)) { continue }
    if (!(Test-Path -LiteralPath $path)) { throw "Incomplete native stage (raw samples missing): $stage" }
    $configPath = Join-Path $directory 'stage-config.json'
    $config = if (Test-Path -LiteralPath $configPath) { Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json } else { $null }
    $samples = @(Get-Content -LiteralPath $path -Raw | ConvertFrom-Json | Where-Object { $null -ne $_ })
    $names = if ($config) { @($config.scenarios) } elseif ($stage -eq 'startup') { @($samples.mode | Select-Object -Unique) } else { @($samples.scenario | Select-Object -Unique) }
    if($config){
        $expectedCount=[int]$config.samples*$names.Count*$(if($config.shared){2}else{1})
        if($samples.Count -ne $expectedCount){throw "Stage $stage has $($samples.Count) launches; expected $expectedCount"}
    }
    foreach ($name in $names) {
        $b = @($samples | Where-Object { $_.binary -eq 'baseline' -and $(if($stage -eq 'startup'){$_.mode}else{$_.scenario}) -eq $name })
        $c = @($samples | Where-Object { $_.binary -eq 'candidate' -and $(if($stage -eq 'startup'){$_.mode}else{$_.scenario}) -eq $name })
        $expected = if ($config) { [int]$config.samples } else { 0 }
        Check-NativeGroup $c $expected "candidate/$stage/$name"
        if ($b.Count -or ($config -and $config.shared)) { Check-NativeGroup $b $expected "baseline/$stage/$name" }
        if ($stage -eq 'startup') {
            $rows.Add((Metric "startup-$name-first-frame" $b.firstFrameMs $c.firstFrameMs 'native-worker' $null))
            if ($name -eq 'uncached') {
                if (@($c.walkDoneMs | Where-Object { $null -eq $_ }).Count -or ($b.Count -and @($b.walkDoneMs | Where-Object { $null -eq $_ }).Count)) { throw 'Uncached startup is missing completed walks' }
                $rows.Add((Metric "startup-$name-complete-walk" $b.walkDoneMs $c.walkDoneMs 'native-worker' $null))
            }
        } else {
            $cf = @($c | ForEach-Object { $_.frameIntervalsMs } | Where-Object { $null -ne $_ })
            $bf = @($b | ForEach-Object { $_.frameIntervalsMs } | Where-Object { $null -ne $_ })
            if ($cf.Count) {
                foreach($launch in @($b)+@($c)){if(@($launch.frameIntervalsMs).Count -ne 120){throw "Expected120 frames in each $name launch"}}
                $rows.Add((Metric "$name-frames" $bf $cf 'native-frame' $null))
                $bm = @($b | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
                $cm = @($c | ForEach-Object { ($_.frameIntervalsMs | Measure-Object -Average).Average })
                $rows.Add((Metric "$name-launch-means" $bm $cm 'native-launch-mean' $null))
            } elseif ($name -in 'scroll','search-scroll','settings-tabs','general-tabs','branch-sort','graph-hover') { throw "Missing measured frames: $name" }
            if (@($c.workerMs | Where-Object { $null -ne $_ }).Count) { $rows.Add((Metric "$name-request-to-frame" $b.workerMs $c.workerMs 'native-worker' $null)) }
            if($name -eq 'general-settings'){
                foreach($field in 'currentCheckMs','availableCheckMs','errorCheckMs','cacheClearFrameMs'){
                    foreach($launch in @($b)+@($c)){
                        if($null -eq $launch.generalMetrics -or $null -eq $launch.generalMetrics.$field){throw "Missing $field in a General settings launch"}
                    }
                    $before=@($b|ForEach-Object{$_.generalMetrics.$field})
                    $after=@($c|ForEach-Object{$_.generalMetrics.$field})
                    $rows.Add((Metric "general-settings-$field" $before $after 'native-worker' $null))
                }
            }
            $handlers = @($c | ForEach-Object { $_.todoHandlerMs } | Where-Object { $null -ne $_ })
            if($handlers.Count){
                if($handlers.Count -ne 120*$c.Count){throw "Expected120 actual handlers per $name launch"}
                $bh=@($b|ForEach-Object{$_.todoHandlerMs}|Where-Object{$null -ne $_})
                $rows.Add((Metric "$name-handler-cpu" $bh $handlers 'native-handler' $null))
                $bm=@($b|ForEach-Object{($_.todoHandlerMs|Measure-Object -Average).Average})
                $cm=@($c|ForEach-Object{($_.todoHandlerMs|Measure-Object -Average).Average})
                $rows.Add((Metric "$name-handler-launch-means" $bm $cm 'native-handler-launch-mean' $null))
            }
        }
    }
    $stages.Add([pscustomobject]@{stage=$stage;retainedLaunches=$samples.Count;configured=[bool]$config;expectedSamplesPerGroup=if($config){$config.samples}else{$null}})
}
function Add-Measure($Map,[string]$Key,$Dimensions,[int]$Sample,$Value) {
    if ($Value -isnot [ValueType] -or $Value -is [bool]) { throw "Non-numeric component value: $Key" }
    if (!$Map.ContainsKey($Key)) { $Map.Add($Key,[pscustomobject]@{dimensions=$Dimensions;values=[Collections.Generic.List[double]]::new();samples=[Collections.Generic.List[int]]::new()}) }
    $Map[$Key].values.Add([double]$Value); $Map[$Key].samples.Add($Sample)
}
function Read-Measures([string]$Path) {
    $text = Get-Content -LiteralPath $Path -Raw
    if ($text -notmatch 'running 1 test' -or $text -notmatch 'test result: ok\. 1 passed;') { throw "Unsuccessful/missing harness: $Path" }
    # Ordinal keys deliberately keep case-distinct branch groups Case/case apart.
    $result = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)
    $records = [Collections.Generic.List[object]]::new()
    foreach ($match in [regex]::Matches($text,'(?m)^\{[^\r\n]*\}\r?$')) { $records.Add(($match.Value | ConvertFrom-Json)) }
    foreach ($match in [regex]::Matches($text,'(?ms)^\[\s*\r?\n.*?^\]')) { foreach($record in @($match.Value | ConvertFrom-Json)){$records.Add($record)} }
    if ($text -match '(?m)branch tips; one warm-up, 15 samples: (\[[^\r\n]*\])') { foreach($record in @($Matches[1]|ConvertFrom-Json)){$records.Add($record)} }
    foreach ($record in $records) {
        if (!($record.PSObject.Properties.Name -contains 'sample') -or $record.sample -lt 0) { continue }
        $dimensions = [ordered]@{}
        foreach ($field in 'entries','backend','distribution','branch','dirty','operation','visibleRows','annotated') {
            if ($record.PSObject.Properties.Name -contains $field) { $dimensions[$field] = $record.$field }
        }
        $prefix = (@($dimensions.Keys | ForEach-Object { "$_=$([Uri]::EscapeDataString([string]$dimensions[$_]))" }) -join ',')
        foreach ($field in $record.PSObject.Properties | Where-Object Name -Match '(?i)ms$') {
            $key = if($prefix){"$prefix-$($field.Name)"}else{$field.Name}
            Add-Measure $result $key $dimensions ([int]$record.sample) $field.Value
        }
    }
    foreach ($match in [regex]::Matches($text,'(?m)^(provider_[a-z0-9_]+)=(\[[^\r\n]*\])\r?$')) {
        $values = @($match.Groups[2].Value | ConvertFrom-Json)
        for($i=0;$i -lt $values.Count;$i++){Add-Measure $result $match.Groups[1].Value ([ordered]@{}) $i $values[$i]}
    }
    if (!$result.Count) { throw "No recognized measurements: $Path" }
    foreach ($key in $result.Keys) {
        $group = $result[$key]; $ids = @($group.samples | Sort-Object -Unique)
        if ($group.values.Count -ne 15 -or $ids.Count -ne 15 -or ($ids[0] -ne 0 -and $ids[0] -ne 1) -or $ids[-1]-$ids[0] -ne 14) { throw "Expected15 unique retained samples for $key ($Path)" }
    }
    return ,$result
}
$componentDirectory = Join-Path $OutputDirectory 'components'
$prototype = [Collections.Generic.List[object]]::new()
if (Test-Path -LiteralPath $componentDirectory) {
    $configPath = Join-Path $componentDirectory 'harness-config.json'
    if (Test-Path -LiteralPath $configPath) {
        $config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
        foreach ($test in @($config.shared)+@($config.candidateOnly)) {
            foreach($role in $(if($test -in $config.shared){@('baseline','candidate')}else{@('candidate')})){
                $file=Join-Path $componentDirectory "$role-$($test -replace '::','-').txt"
                if(!(Test-Path -LiteralPath $file)){throw "Missing expected component harness: $file"}
            }
        }
    }
    foreach ($file in Get-ChildItem -LiteralPath $componentDirectory -Filter 'candidate-*.txt') {
        $name = $file.BaseName.Substring('candidate-'.Length); $candidate = Read-Measures $file.FullName
        $baselinePath = Join-Path $componentDirectory ("baseline-"+$file.Name.Substring('candidate-'.Length))
        $baseline = if(Test-Path -LiteralPath $baselinePath){Read-Measures $baselinePath}else{[Collections.Generic.Dictionary[string,object]]::new([StringComparer]::Ordinal)}
        foreach($key in $baseline.Keys){if(!$candidate.ContainsKey($key)){throw "Candidate omitted shared measurement $name/$key"}}
        foreach ($key in $candidate.Keys | Sort-Object) {
            $before = if($baseline.ContainsKey($key)){@($baseline[$key].values)}else{@()}
            $rows.Add((Metric "$name-$key" $before @($candidate[$key].values) 'component' $candidate[$key].dimensions))
            if ($name -eq 'branches-tests-benchmark_activity_recency_prototype' -and $key.EndsWith('-weightedMs',[StringComparison]::Ordinal)) {
                $oldKey = $key.Substring(0,$key.Length-'weightedMs'.Length)+'oldCountMs'
                if(!$candidate.ContainsKey($oldKey)){throw 'Recency prototype missing its paired old-count control'}
                $paired = Metric "prototype-$key" @($candidate[$oldKey].values) @($candidate[$key].values) 'prototype-paired' $candidate[$key].dimensions
                $paired.comparisonKind='prototype-paired'; $prototype.Add($paired)
            }
        }
    }
    $stages.Add([pscustomobject]@{stage='components';harnessLogs=@(Get-ChildItem -LiteralPath $componentDirectory -Filter '*.txt').Count;retainedSamplesPerGroup=15})
}
ConvertTo-Json -InputObject @($rows) -Depth 7 | Set-Content (Join-Path $OutputDirectory 'summary.json')
ConvertTo-Json -InputObject @($stages) -Depth 5 | Set-Content (Join-Path $OutputDirectory 'stage-completeness.json')
ConvertTo-Json -InputObject @($prototype) -Depth 7 | Set-Content (Join-Path $OutputDirectory 'prototype-recency-summary.json')
$lines=@('| Metric | Baseline median/p95 ms | Candidate median/p95 ms | Median delta ms / % | p95 delta ms / % | Samples B/C |','| --- | ---: | ---: | ---: | ---: | ---: |')
foreach($row in $rows){
    $before=if($row.baselineSamples){'{0:F4}/{1:F4}' -f $row.baselineMedianMs,$row.baselineP95Ms}else{'candidate only'}
    $md=if($row.baselineSamples){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.medianDeltaMs,$row.medianDeltaPercent}else{'n/a'}
    $pd=if($row.baselineSamples){'{0:+0.0000;-0.0000;0.0000} / {1:+0.00;-0.00;0.00}%' -f $row.p95DeltaMs,$row.p95DeltaPercent}else{'n/a'}
    $lines+='| {0} | {1} | {2:F4}/{3:F4} | {4} | {5} | {6}/{7} |' -f $row.metric,$before,$row.candidateMedianMs,$row.candidateP95Ms,$md,$pd,$row.baselineSamples,$row.candidateSamples
}
$lines+='';$lines+='Nearest-rank percentiles; p95 with15 samples is the maximum. Frames within a native launch are correlated; inspect launch means. Shared native launches alternate order; internally warmed component binaries run as separate blocks, reversing order between harnesses. entries/backend/distribution/branch/dirty/operation groups are separate, including case-distinct branch names. Prototype old-count/weighted pairs are test-only controls in prototype-recency-summary.json, not production baseline comparisons or an authorization to change ranking. Candidate-only costs have no baseline equivalent. Native bundled request-to-frame costs include the whole named scenario; component I/O/CPU and process wait exclude physical input/display scanout. Inspect stage-completeness.json and raw logs; this table is not a zero-regression guarantee or automatic acceptance decision.'
$lines|Set-Content (Join-Path $OutputDirectory 'summary.md')
$rows|Format-Table metric,baselineMedianMs,candidateMedianMs,baselineP95Ms,candidateP95Ms,medianDeltaPercent
