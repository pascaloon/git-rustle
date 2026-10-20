param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Actions','Components')][string[]]$Stages = @('Startup','History','Actions','Components'),
    [ValidateSet('todo-graph','branch-sort','graph-hover','stash-panel','stash-apply','stash-delete','external-tools','ref-storage')]
    [string[]]$ActionScenarios = @('todo-graph','branch-sort','graph-hover','stash-panel','stash-apply','stash-delete','external-tools','ref-storage'),
    [string[]]$SharedComponentTests = @(
        'branches::tests::benchmark_browser','branches::tests::benchmark_owner_filter','search::tests::benchmark_search',
        'remote::requests::integration::tests::benchmark_provider_cache','desktop::provider::tests::benchmark_provider_lookup',
        'remote::requests::integration::tests::benchmark_gitlab_project_icon','engine::tests::benchmark_desktop_mutations',
        'git::operations::tests::benchmark_large_worktree_operations'),
    [string[]]$CandidateComponentTests = @(
        'graph::hover::tests::benchmark_hover','branches::tests::benchmark_header_sort','branches::tests::benchmark_activity_recency_prototype',
        'git::refs::tests::benchmark_reference_storage','git::checkout::tests::benchmark_case_distinct_checkout',
        'remote::requests::integration::tests::benchmark_project_icon_reopen','remote::requests::target::tests::benchmark_target_inference',
        'desktop::provider::tests::benchmark_provider_search_projection','external_tools::tests::benchmark_external_tools',
        'git::operations::tests::benchmark_stashes')
)
# Run preserved executables only. Fixture preparation and integrity checks occur
# outside measured native intervals; all results and disposable mutations stay ignored.
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
foreach($name in 'Repository','Baseline','BaselineHarness','CandidateHarness','Candidate'){
    Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path
}
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$allowed=Join-Path $sourceRoot 'target/bench'
if(!$OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Todo results must stay under this workspace target/bench/<run>'}
$saved=@{}
function Save-Env([string]$Name){if(!$saved.ContainsKey($Name)){$saved[$Name]=[Environment]::GetEnvironmentVariable($Name,'Process')}}
foreach($name in 'LOCALAPPDATA','TEMP','TMP','GIT_OPTIONAL_LOCKS','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_BENCH_REPO','GIT_RUSTLE_BENCH_BRANCHES','GIT_CONFIG_COUNT'){Save-Env $name}
function Assert-Idle { if(@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count){throw 'Timing rejected: finish builds before measuring'} }
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
$invocation=[Guid]::NewGuid().ToString('N')
$beforeFiles=$null;$beforeFixture=$null;$executables=@();$success=$false
try{
    Assert-Idle
    $env:LOCALAPPDATA=Join-Path $OutputDirectory 'app-data'
    $env:TEMP=$env:TMP=Join-Path $OutputDirectory 'temp'
    $fixtureRoot=Join-Path $OutputDirectory 'disposable-fixtures'
    Isolated-Directory $env:LOCALAPPDATA '.git-rustle-bench-app-data' 'disposable todo app data'
    Isolated-Directory $env:TEMP '.git-rustle-bench-temp' 'disposable todo temporary files'
    Isolated-Directory $fixtureRoot '.git-rustle-benchmark-fixtures' 'disposable git-rustle todo fixtures'
    [IO.File]::WriteAllText((Join-Path $env:LOCALAPPDATA 'cache-benchmark-fixture'),'disposable image cache benchmark')
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
    [pscustomobject]@{recordedUtc=[DateTime]::UtcNow.ToString('o');sourceHead=(Git-Read $sourceRoot @('rev-parse','HEAD'));sourceStatus=(Git-Read $sourceRoot @('status','--porcelain=v1'))
        repository=$Repository;fixtureBefore=$beforeFixture;sourceFiles=$beforeFiles;executables=$executables;samples=$Samples;stages=$Stages;actions=$ActionScenarios
        sharedComponents=$SharedComponentTests;candidateComponents=$CandidateComponentTests;cache=$env:LOCALAPPDATA;temp=$env:TEMP;fixtureRoot=$fixtureRoot
        machine=[Environment]::MachineName;os=[Environment]::OSVersion.ToString();logicalProcessors=[Environment]::ProcessorCount
        order='Native shared launches alternate. Components use their own warm-up+15 samples in separate binary blocks, reversing order between shared harnesses.'
    }|ConvertTo-Json -Depth 7|Set-Content (Join-Path $OutputDirectory "provenance-$invocation.json")
    if('Startup'-in$Stages){
        $env:GIT_RUSTLE_SCENARIO='startup-local'
        & "$PSScriptRoot/benchmark.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'startup' @('warm','uncached') $true)
        Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    }
    if('History'-in$Stages){
        $history=@('scroll','search-scroll','branches','search','diff','settings-tabs','general-tabs','general-settings')
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -Scenarios $history -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'history' $history $true)
    }
    if('Actions'-in$Stages){
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate -Scenarios $ActionScenarios -DisposableFixtureRoot $fixtureRoot -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'actions' $ActionScenarios $false)
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
    & "$PSScriptRoot/summarize-todos.ps1" -OutputDirectory $OutputDirectory
    $success=$true
}finally{
    try{
        if($null-ne$beforeFiles){
            $afterFiles=@(Source-Files);$afterFixture=Fixture-Identity
            $sourceSame=(ConvertTo-Json -InputObject @($beforeFiles) -Compress)-ceq(ConvertTo-Json -InputObject @($afterFiles) -Compress)
            $fixtureSame=(ConvertTo-Json -InputObject $beforeFixture -Compress)-ceq(ConvertTo-Json -InputObject $afterFixture -Compress)
            $changedBinaries=@($executables|Where-Object{(Get-FileHash -LiteralPath $_.path).Hash-cne$_.sha256})
            [pscustomobject]@{completed=$success;sourceUnchanged=$sourceSame;fixtureUnchanged=$fixtureSame;fixtureBefore=$beforeFixture;fixtureAfter=$afterFixture;changedBinaries=$changedBinaries;sourceAfter=$afterFiles}|ConvertTo-Json -Depth 7|Set-Content (Join-Path $OutputDirectory "integrity-$invocation.json")
            if(!$sourceSame-or!$fixtureSame-or$changedBinaries.Count){throw 'Integrity changed during benchmarks; retain raw output and investigate'}
        }
    }finally{foreach($name in $saved.Keys){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
}
