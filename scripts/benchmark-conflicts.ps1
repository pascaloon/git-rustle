param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Native','Components')][string[]]$Stages = @('Startup','History','Native','Components'),
    [string[]]$SharedComponentTests = @('git::operations::tests::benchmark_stashes'),
    [string[]]$CandidateComponentTests = @('git::operations::tests::benchmark_conflict_refresh')
)
# Run preserved executables only. Fixture preparation and integrity checks occur
# outside measured native intervals; all results and disposable mutations stay ignored.
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
$scriptsRoot = Join-Path $sourceRoot 'scripts'
$runnerPath=$PSCommandPath; $runnerHash=(Get-FileHash -LiteralPath $runnerPath).Hash
$summaryPath=Join-Path $PSScriptRoot 'summarize-conflicts.ps1'; $summaryHash=(Get-FileHash -LiteralPath $summaryPath).Hash
. (Join-Path $scriptsRoot 'benchmark-processes.ps1')
foreach($name in 'Repository','Baseline','BaselineHarness','CandidateHarness','Candidate'){
    Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$allowed=Join-Path $sourceRoot 'target/bench'
if(!$OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Conflict results must stay under this workspace target/bench/<run>'}
$saved=@{}
function Save-Env([string]$Name){if(!$saved.ContainsKey($Name)){$saved[$Name]=[Environment]::GetEnvironmentVariable($Name,'Process')}}
foreach($name in 'LOCALAPPDATA','TEMP','TMP','GIT_OPTIONAL_LOCKS','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_BENCH_REPO','GIT_RUSTLE_BENCH_BRANCHES','GIT_CONFIG_COUNT','GIT_RUSTLE_TOOL_DISCOVERY_ROOTS','GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES'){Save-Env $name}
function Assert-Idle { Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
function Git-Read([string]$Path,[string[]]$Arguments){
    $lines=@(& git -c "safe.directory=$($Path.Replace('\','/'))" -C $Path @Arguments 2>&1)
    if($LASTEXITCODE-ne0){throw "Git provenance failed: $Arguments"}
    return ($lines-join"`n").TrimEnd()
}
function Source-Files {
    $files=@(Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'src'),(Join-Path $sourceRoot 'scripts'),(Join-Path $sourceRoot 'docs') -Recurse -File)
    $files+=@(foreach($file in 'README.md','Cargo.toml','Cargo.lock','build.rs'){if(Test-Path -LiteralPath (Join-Path $sourceRoot $file)){Get-Item -LiteralPath (Join-Path $sourceRoot $file)}})
    return @($files|Sort-Object FullName|ForEach-Object{[pscustomobject]@{path=[IO.Path]::GetRelativePath($sourceRoot,$_.FullName);sha256=(Get-FileHash -LiteralPath $_.FullName).Hash}})
}
function Fixture-Identity {
    [pscustomobject]@{head=(Git-Read $Repository @('rev-parse','HEAD'));refs=(Git-Read $Repository @('for-each-ref','--format=%(refname) %(objectname)'));status=(Git-Read $Repository @('status','--porcelain=v1'))}
}
function Isolated-Directory([string]$Path,[string]$Marker,[string]$Text){
    if(Test-Path -LiteralPath $Path){
        if(!(Test-Path -LiteralPath (Join-Path $Path $Marker)) -or [IO.File]::ReadAllText((Join-Path $Path $Marker)) -cne $Text){throw "Refusing unmarked existing benchmark directory: $Path"}
    }else{New-Item -ItemType Directory -Path $Path|Out-Null}
    [IO.File]::WriteAllText((Join-Path $Path $Marker),$Text)
}
function New-Stage([string]$Name,[string[]]$Scenarios,[bool]$Shared){
    $path=Join-Path $OutputDirectory $Name
    if(Test-Path -LiteralPath $path){throw "Retaining existing $Name results; omit completed stages or choose a new output directory"}
    New-Item -ItemType Directory -Path $path|Out-Null
    [pscustomobject]@{stage=$Name;samples=$Samples;scenarios=$Scenarios;shared=$Shared}|ConvertTo-Json -Depth 4|Set-Content (Join-Path $path 'stage-config.json')
    return $path
}
function Invoke-Harness([string]$Exe,[string]$Test,[string]$Log){
    if($Test -notmatch '^[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)+$'){throw 'Use an exact Rust harness module/test name'}
    Assert-Idle
    & $Exe $Test --ignored --exact --nocapture 2>&1|Tee-Object -FilePath $Log
    if($LASTEXITCODE-ne0){throw "Harness failed; raw output retained: $Test ($Log)"}
    Assert-Idle
    $text=Get-Content -LiteralPath $Log -Raw
    if($text-notmatch'running 1 test'-or$text-notmatch'test result: ok\. 1 passed;'){throw "Expected exactly one passing harness: $Log"}
}
function Fixture-Git([string]$Path,[string[]]$Arguments){
    $lines=@(& git -c "safe.directory=$($Path.Replace('\','/'))" -c core.hooksPath= -c core.autocrlf=false -C $Path @Arguments 2>&1)
    if($LASTEXITCODE-ne0){throw "Conflict fixture Git failed ($Arguments): $($lines-join' ')"}
    return($lines-join"`n").TrimEnd()
}
function New-ConflictFixture([string]$Case,[int]$Sample){
    $path=Join-Path $fixtureRoot "$Case-$Sample"
    if(Test-Path -LiteralPath $path){throw 'Retaining existing native conflict fixture'}
    New-Item -ItemType Directory -Path $path|Out-Null
    Fixture-Git $path @('init','-b','main')|Out-Null
    foreach($setting in @(@('user.name','Conflict benchmark'),@('user.email','conflicts@example.invalid'),@('core.autocrlf','false'),@('core.hooksPath','disabled-test-hooks'),@('commit.gpgSign','false'),@('tag.gpgSign','false'),@('core.editor','true'),@('sequence.editor','true'),@('rerere.enabled','false'))){Fixture-Git $path @('config',$setting[0],$setting[1])|Out-Null}
    foreach($name in 'a.txt','b.txt','c.txt'){[IO.File]::WriteAllText((Join-Path $path $name),"base`n")}
    Fixture-Git $path @('add','.')|Out-Null
    Fixture-Git $path @('commit','--no-gpg-sign','-m','Base')|Out-Null
    Fixture-Git $path @('switch','-c','topic')|Out-Null
    foreach($name in 'a.txt','b.txt','c.txt'){[IO.File]::WriteAllText((Join-Path $path $name),"theirs`n")}
    if($Case-eq'stash'){
        Fixture-Git $path @('stash','push','-m','Incoming stash')|Out-Null
        Fixture-Git $path @('switch','main')|Out-Null
    }else{
        Fixture-Git $path @('commit','--no-gpg-sign','-am','Incoming topic')|Out-Null
        Fixture-Git $path @('switch','main')|Out-Null
    }
    foreach($name in 'a.txt','b.txt','c.txt'){[IO.File]::WriteAllText((Join-Path $path $name),"ours`n")}
    Fixture-Git $path @('commit','--no-gpg-sign','-am','Local change')|Out-Null
    [IO.File]::WriteAllText((Join-Path $path '.git/git-rustle-conflicts-fixture'),'disposable conflict benchmark fixture')
    $state=[pscustomobject]@{repository=$path;case=$Case;head=(Fixture-Git $path @('rev-parse','HEAD'));topic=(Fixture-Git $path @('rev-parse','topic'));stashes=(Fixture-Git $path @('stash','list','--format=%H'));refs=(Fixture-Git $path @('for-each-ref','--format=%(refname) %(objectname)'));status=(Fixture-Git $path @('status','--porcelain=v1'))}
    if($state.status){throw 'Conflict fixture must start clean before actual UI operation setup'}
    return $state
}
function Invoke-ConflictNative([string]$Case,[int]$Sample,[string]$Output){
    Assert-Idle
    $fixture=New-ConflictFixture $Case $Sample
    $scenario="conflicts-$Case";$prefix="candidate-$scenario-$Sample"
    $fixture|ConvertTo-Json -Depth 4|Set-Content (Join-Path $Output "$prefix-before.json")
    $log=Join-Path $Output "$prefix.txt"
    $env:GIT_RUSTLE_SCENARIO=$scenario;$env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_AUTOQUIT='100'
    $env:GIT_RUSTLE_DUMP=Join-Path $Output "$prefix-state.txt"
    # This is the native UI validation window: visible presentation is required for verified GPUI paints.
    $process=Start-Process -FilePath $Candidate -ArgumentList @('"'+$fixture.repository+'"') -WindowStyle Normal -RedirectStandardError $log -PassThru
    if(!$process.WaitForExit(65000)){Stop-Process -Id $process.Id;throw "Conflict native timeout; retained $log"}
    $process.Refresh();if($process.ExitCode-ne0){throw "Conflict native failure; retained $log"}
    Assert-Idle
    $state=Get-Content -LiteralPath $env:GIT_RUSTLE_DUMP -Raw
    if($state-notmatch'conflicts_verified=true'-or$state-notmatch"conflicts_kind=$Case(?:\s|$)"-or$state-notmatch'conflicts_checks=[1-9][0-9]*'){throw 'Native conflict UI did not verify required actions'}
    $head=Fixture-Git $fixture.repository @('rev-parse','HEAD')
    $branch=Fixture-Git $fixture.repository @('symbolic-ref','--short','HEAD')
    $topic=Fixture-Git $fixture.repository @('rev-parse','topic')
    $stashes=Fixture-Git $fixture.repository @('stash','list','--format=%H')
    $unmerged=Fixture-Git $fixture.repository @('ls-files','-u')
    $status=Fixture-Git $fixture.repository @('status','--porcelain=v1')
    if($branch-cne'main'-or$topic-cne$fixture.topic-or$stashes-cne$fixture.stashes-or$unmerged){throw 'Conflict final branch/topic/stash/index postconditions failed'}
    $contents=@{}
    foreach($name in 'a.txt','b.txt','c.txt'){$contents[$name]=[IO.File]::ReadAllText((Join-Path $fixture.repository $name))}
    if($Case-eq'abort'){
        if($head-cne$fixture.head-or$status-or@($contents.Values|Where-Object{$_-cne"ours`n"}).Count){throw 'Abort did not restore original clean local tree/HEAD'}
    }else{
        if($contents['a.txt']-cne"theirs`n"-or$contents['b.txt']-cne"resolved editor`n"-or$contents['c.txt']-cne"resolved external`n"){throw 'Conflict resolutions did not preserve incoming/editor/external content'}
        if($Case-eq'stash'){if($head-cne$fixture.head-or!$status){throw 'Stash finish must retain HEAD and pending resolved changes'}}
        elseif($head-ceq$fixture.head-or$status){throw 'Merge/rebase Continue must produce a new clean HEAD'}
    }
    foreach($operationPath in 'MERGE_HEAD','rebase-merge','rebase-apply','rustle-operations/stash-apply'){if(Test-Path -LiteralPath (Join-Path $fixture.repository ".git/$operationPath")){throw 'Conflict operation state remains after Continue/Abort'}}
    [pscustomobject]@{case=$Case;sample=$Sample;repository=$fixture.repository;head=$head;branch=$branch;topic=$topic;stashes=$stashes;unmerged=$unmerged;status=$status;contents=$contents}|ConvertTo-Json -Depth 4|Set-Content (Join-Path $Output "$prefix-after.json")
    $marks=@{}
    foreach($mark in [regex]::Matches((Get-Content -LiteralPath $log -Raw),'([\d.]+) ms  ([^\r\n]+)')){if(!$marks.ContainsKey($mark.Groups[2].Value)){$marks[$mark.Groups[2].Value]=[double]::Parse($mark.Groups[1].Value,[cultureinfo]::InvariantCulture)}}
    $ops=if($Case-eq'abort'){@('open','abort')}else{@('open','incoming','local','whole','file','stale','reject','reload','save','external','continue')}
    $metrics=[ordered]@{}
    foreach($op in $ops){
        $begin="conflicts $op requested";$end="conflicts $op frame"
        if(!$marks.ContainsKey($begin)-or!$marks.ContainsKey($end)){throw "Missing $op interval in $log"}
        $metrics[$op+'Ms']=$marks[$end]-$marks[$begin]
        if($metrics[$op+'Ms']-lt0){throw 'Nonmonotonic conflict interval'}
    }
    if(!$marks.ContainsKey('conflicts final frame')){throw 'Missing final conflict frame'}
    [pscustomobject]@{binary='candidate';scenario=$scenario;sample=$Sample;frameIntervalsMs=$null;workerMs=$marks['conflicts final frame']-$marks['conflicts open requested'];conflictMetrics=[pscustomobject]$metrics}
}
$invocation=[Guid]::NewGuid().ToString('N')
$beforeFiles=$null;$beforeFixture=$null;$executables=@();$success=$false
try{
    Assert-Idle
    $env:LOCALAPPDATA=Join-Path $OutputDirectory 'app-data'
    $env:TEMP=$env:TMP=Join-Path $OutputDirectory 'temp'
    $fixtureRoot=Join-Path $OutputDirectory 'native-fixtures'
    Isolated-Directory $env:LOCALAPPDATA '.git-rustle-bench-app-data' 'disposable conflict app data'
    Isolated-Directory $env:TEMP '.git-rustle-bench-temp' 'disposable conflict temporary files'
    Isolated-Directory $fixtureRoot '.git-rustle-conflicts-fixtures' 'disposable conflict fixtures'
    $configRoot=Join-Path $env:LOCALAPPDATA 'git-rustle'
    New-Item -ItemType Directory -Path $configRoot -Force|Out-Null
    [IO.File]::WriteAllText((Join-Path $configRoot 'tools.json'),'{"scanned":true,"tools":[]}')
    Remove-Item Env:GIT_RUSTLE_TOOL_DISCOVERY_ROOTS -ErrorAction SilentlyContinue
    foreach($name in 'GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES'){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}
    $env:GIT_OPTIONAL_LOCKS='0'
    $count=0
    if($env:GIT_CONFIG_COUNT -and (![int]::TryParse($env:GIT_CONFIG_COUNT,[ref]$count)-or$count-lt0)){throw 'Invalid inherited GIT_CONFIG_COUNT'}
    foreach($safe in @($sourceRoot,$Repository,($env:TEMP+'/*'),($fixtureRoot+'/*'))){
        Save-Env "GIT_CONFIG_KEY_$count";Save-Env "GIT_CONFIG_VALUE_$count"
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_KEY_$count",'safe.directory','Process')
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_VALUE_$count",$safe.Replace('\','/'),'Process');$count++
    }
    $env:GIT_CONFIG_COUNT="$count"
    $env:GIT_RUSTLE_BENCH_REPO=$Repository;$env:GIT_RUSTLE_BENCH_BRANCHES='1000'
    Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    $beforeFiles=@(Source-Files);$beforeFixture=Fixture-Identity
    $executables=@(foreach($name in 'Baseline','BaselineHarness','Candidate','CandidateHarness'){
        $path=Get-Variable $name -ValueOnly
        [pscustomobject]@{role=$name;path=$path;bytes=(Get-Item -LiteralPath $path).Length;sha256=(Get-FileHash -LiteralPath $path).Hash}
    })
    [pscustomobject]@{recordedUtc=[DateTime]::UtcNow.ToString('o');runner=$runnerPath;runnerSha256=$runnerHash;summarizer=$summaryPath;summarizerSha256=$summaryHash;sourceHead=(Git-Read $sourceRoot @('rev-parse','HEAD'));sourceStatus=(Git-Read $sourceRoot @('status','--porcelain=v1'))
        repository=$Repository;fixtureBefore=$beforeFixture;sourceFiles=$beforeFiles;executables=$executables;samples=$Samples;stages=$Stages;actions=@('conflicts-merge','conflicts-rebase','conflicts-stash','conflicts-abort')
        sharedComponents=$SharedComponentTests;candidateComponents=$CandidateComponentTests;cache=$env:LOCALAPPDATA;temp=$env:TEMP;fixtureRoot=$fixtureRoot
        machine=[Environment]::MachineName;os=[Environment]::OSVersion.ToString();logicalProcessors=[Environment]::ProcessorCount;backgroundProcessAudit=(Join-Path $OutputDirectory 'background-processes.json')
        order='Native shared launches alternate. Components use their own warm-up+15 samples in separate binary blocks, reversing order between shared harnesses.'
    }|ConvertTo-Json -Depth 7|Set-Content (Join-Path $OutputDirectory "provenance-$invocation.json")
    if('Startup'-in$Stages){
        $env:GIT_RUSTLE_SCENARIO='startup-local'
        & (Join-Path $scriptsRoot 'benchmark.ps1') -Repository $Repository -Baseline $Baseline -Candidate $Candidate -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'startup' @('warm','uncached') $true)
        Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    }
    if('History'-in$Stages){
        $scenarios=@('scroll','search-scroll')
        & (Join-Path $scriptsRoot 'benchmark-desktop.ps1') -Repository $Repository -Baseline $Baseline -Candidate $Candidate -Scenarios $scenarios -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'history' $scenarios $true)
    }
    if('Native'-in$Stages){
        $actions=New-Stage 'actions' @('conflicts-merge','conflicts-rebase','conflicts-stash','conflicts-abort') $false
        $nativeSamples=[Collections.Generic.List[object]]::new()
        foreach($case in 'merge','rebase','stash','abort'){
            Invoke-ConflictNative $case -1 $actions|Out-Null
            for($sample=0;$sample-lt$Samples;$sample++){$nativeSamples.Add((Invoke-ConflictNative $case $sample $actions))}
        }
        ConvertTo-Json -InputObject @($nativeSamples) -Depth 6|Set-Content (Join-Path $actions 'samples.json')
    }
    if('Components'-in$Stages){
        $components=New-Stage 'components' @() $false
        [pscustomobject]@{shared=$SharedComponentTests;candidateOnly=$CandidateComponentTests;retainedSamplesPerGroup=15}|ConvertTo-Json -Depth 4|Set-Content (Join-Path $components 'harness-config.json')
        for($i=0;$i-lt$SharedComponentTests.Count;$i++){
            $test=$SharedComponentTests[$i];$roles=if($i%2-eq0){@('baseline','candidate')}else{@('candidate','baseline')}
            foreach($role in $roles){$exe=if($role-eq'baseline'){$BaselineHarness}else{$CandidateHarness};Invoke-Harness $exe $test (Join-Path $components "$role-$($test-replace'::','-').txt")}
        }
        foreach($test in $CandidateComponentTests){Invoke-Harness $CandidateHarness $test (Join-Path $components "candidate-$($test-replace'::','-').txt")}
    }
    Assert-Idle
    & "$PSScriptRoot/summarize-conflicts.ps1" -OutputDirectory $OutputDirectory
    $success=$true
}finally{
    try{
        if($null-ne$beforeFiles){
            $afterFiles=@(Source-Files);$afterFixture=Fixture-Identity
            $sourceSame=(ConvertTo-Json -InputObject @($beforeFiles) -Compress)-ceq(ConvertTo-Json -InputObject @($afterFiles) -Compress)
            $fixtureSame=(ConvertTo-Json -InputObject $beforeFixture -Compress)-ceq(ConvertTo-Json -InputObject $afterFixture -Compress)
            $changedBinaries=@($executables|Where-Object{(Get-FileHash -LiteralPath $_.path).Hash-cne$_.sha256})
            [pscustomobject]@{completed=$success;sourceUnchanged=$sourceSame;runnerUnchanged=((Get-FileHash -LiteralPath $runnerPath).Hash-ceq$runnerHash);summarizerUnchanged=((Get-FileHash -LiteralPath $summaryPath).Hash-ceq$summaryHash);fixtureUnchanged=$fixtureSame;fixtureBefore=$beforeFixture;fixtureAfter=$afterFixture;changedBinaries=$changedBinaries;sourceAfter=$afterFiles}|ConvertTo-Json -Depth 7|Set-Content (Join-Path $OutputDirectory "integrity-$invocation.json")
            if(!$sourceSame-or!$fixtureSame-or$changedBinaries.Count-or(Get-FileHash -LiteralPath $runnerPath).Hash-cne$runnerHash-or(Get-FileHash -LiteralPath $summaryPath).Hash-cne$summaryHash){throw 'Integrity changed during benchmarks; retain raw output and investigate'}
        }
    }finally{foreach($name in $saved.Keys){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
}
