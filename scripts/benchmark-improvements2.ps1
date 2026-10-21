param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Actions','Components')][string[]]$Stages = @('Startup','History','Actions','Components')
)
$ErrorActionPreference='Stop'
$sourceRoot=(Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
. "$PSScriptRoot/benchmark-processes.ps1"
foreach($name in 'Repository','Baseline','BaselineHarness','Candidate','CandidateHarness'){Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path}
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=Join-Path $sourceRoot 'target/bench'
if(!$OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Results must stay under workspace target/bench/<run>'}
New-Item -ItemType Directory -Path $OutputDirectory -Force|Out-Null
$saved=@{}
function Save-Env([string]$Name){if(!$saved.ContainsKey($Name)){$saved[$Name]=[Environment]::GetEnvironmentVariable($Name,'Process')}}
foreach($name in 'LOCALAPPDATA','TEMP','TMP','GIT_OPTIONAL_LOCKS','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_BENCH_REPO','GIT_CONFIG_COUNT','GIT_RUSTLE_TOOL_DISCOVERY_ROOTS','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES'){Save-Env $name}
function Assert-Idle {Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json')}
function Git-Read([string]$Path,[string[]]$Arguments){
    $lines=@(& git -c "safe.directory=$($Path.Replace('\','/'))" -C $Path @Arguments 2>&1)
    if($LASTEXITCODE-ne0){throw "Git provenance failed: $Arguments"}
    return($lines-join"`n").TrimEnd()
}
function Source-Files {
    $files=@(Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'src'),(Join-Path $sourceRoot 'scripts'),(Join-Path $sourceRoot 'docs') -Recurse -File)
    foreach($name in 'Cargo.toml','Cargo.lock','build.rs','README.md'){if(Test-Path -LiteralPath (Join-Path $sourceRoot $name)){$files+=Get-Item -LiteralPath (Join-Path $sourceRoot $name)}}
    @($files|Sort-Object FullName|ForEach-Object{[pscustomobject]@{path=[IO.Path]::GetRelativePath($sourceRoot,$_.FullName);sha256=(Get-FileHash -LiteralPath $_.FullName).Hash}})
}
function Fixture-Identity {[pscustomobject]@{head=(Git-Read $Repository @('rev-parse','HEAD'));refs=(Git-Read $Repository @('for-each-ref','--format=%(refname) %(objectname)'));status=(Git-Read $Repository @('status','--porcelain=v1'))}}
function New-Stage([string]$Name,[string[]]$Scenarios,[bool]$Shared){
    $path=Join-Path $OutputDirectory $Name
    if(Test-Path -LiteralPath $path){throw "Retaining existing stage $Name; omit it from -Stages or choose a new output directory"}
    New-Item -ItemType Directory -Path $path|Out-Null
    [pscustomobject]@{stage=$Name;samples=$Samples;scenarios=$Scenarios;shared=$Shared}|ConvertTo-Json -Depth 4|Set-Content (Join-Path $path 'stage-config.json')
    return $path
}
function Harness([string]$Exe,[string]$Test,[string]$Log){
    Assert-Idle
    & $Exe $Test --ignored --exact --nocapture 2>&1|Tee-Object -FilePath $Log
    if($LASTEXITCODE-ne0){throw "Harness failed: $Log"}
    $text=Get-Content -LiteralPath $Log -Raw
    if($text-notmatch'running 1 test'-or$text-notmatch'test result: ok\. 1 passed;'){throw "Expected exactly one passing harness: $Log"}
    Assert-Idle
}
$shared=@('git::worktree::tests::benchmark_pending','graph::hover::tests::benchmark_hover')
$candidateOnly=@('desktop::hover::tests::benchmark_selected_path','git::graph_order_tests::benchmark_graph_walk','git::graph_order_tests::benchmark_equal_timestamp_frontier','git::worktree::discard::tests::benchmark_discard_file')
$invocation=[Guid]::NewGuid().ToString('N')
$before=$null;$fixtureBefore=$null;$executables=@();$complete=$false
try {
    Assert-Idle
    $env:LOCALAPPDATA=Join-Path $OutputDirectory 'app-data';$env:TEMP=$env:TMP=Join-Path $OutputDirectory 'temp'
    foreach($path in $env:LOCALAPPDATA,$env:TEMP){
        if(Test-Path -LiteralPath $path){if(!(Test-Path -LiteralPath (Join-Path $path '.git-rustle-improvements2'))){throw "Unmarked isolation directory: $path"}}
        else{New-Item -ItemType Directory -Path $path|Out-Null}
        [IO.File]::WriteAllText((Join-Path $path '.git-rustle-improvements2'),'disposable improvements2 benchmark state')
    }
    $configRoot=Join-Path $env:LOCALAPPDATA 'git-rustle';New-Item -ItemType Directory -Path $configRoot -Force|Out-Null
    [IO.File]::WriteAllText((Join-Path $configRoot 'tools.json'),'{"scanned":true,"tools":[]}')
    foreach($name in 'GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_TOOL_DISCOVERY_ROOTS'){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}
    $count=0;if($env:GIT_CONFIG_COUNT-and(![int]::TryParse($env:GIT_CONFIG_COUNT,[ref]$count)-or$count-lt0)){throw 'Invalid inherited GIT_CONFIG_COUNT'}
    foreach($path in @($sourceRoot,$Repository,($env:TEMP+'/*'),($OutputDirectory+'/*'))){
        Save-Env "GIT_CONFIG_KEY_$count";Save-Env "GIT_CONFIG_VALUE_$count"
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_KEY_$count",'safe.directory','Process')
        [Environment]::SetEnvironmentVariable("GIT_CONFIG_VALUE_$count",$path.Replace('\','/'),'Process');$count++
    }
    $env:GIT_CONFIG_COUNT="$count";$env:GIT_OPTIONAL_LOCKS='0';$env:GIT_RUSTLE_BENCH_REPO=$Repository
    $before=@(Source-Files);$fixtureBefore=Fixture-Identity
    $executables=@(foreach($role in 'Baseline','BaselineHarness','Candidate','CandidateHarness'){$path=Get-Variable $role -ValueOnly;[pscustomobject]@{role=$role;path=$path;bytes=(Get-Item -LiteralPath $path).Length;sha256=(Get-FileHash -LiteralPath $path).Hash}})
    [pscustomobject]@{recordedUtc=[DateTime]::UtcNow.ToString('o');sourceHead=(Git-Read $sourceRoot @('rev-parse','HEAD'));sourceStatus=(Git-Read $sourceRoot @('status','--porcelain=v1'));repository=$Repository;fixture=$fixtureBefore;sourceFiles=$before;executables=$executables;samples=$Samples;stages=$Stages;sharedComponents=$shared;candidateComponents=$candidateOnly;machine=[Environment]::MachineName;logicalProcessors=[Environment]::ProcessorCount;order='Shared native launches alternate. Component binary blocks reverse order between shared harnesses. One warm-up plus fifteen retained component samples.'}|ConvertTo-Json -Depth 8|Set-Content (Join-Path $OutputDirectory "provenance-$invocation.json")
    if('Startup'-in$Stages){
        $path=New-Stage 'startup' @('warm','uncached') $true
        $env:GIT_RUSTLE_SCENARIO='startup-local'
        & "$PSScriptRoot/benchmark.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -NativeBaseline -NativeCandidate -IsolatedCacheRoot (Join-Path $OutputDirectory 'startup-caches') -RejectCompetingBuilds -Samples $Samples -OutputDirectory $path
        Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    }
    if('History'-in$Stages){
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -Scenarios @('scroll','search-scroll','graph-hover','status-panels') -RequireSelectedPath -Samples $Samples -RejectCompetingBuilds -OutputDirectory (New-Stage 'history' @('scroll','search-scroll','graph-hover','status-panels') $true)
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate -Scenarios @('refresh-feedback') -Samples $Samples -RejectCompetingBuilds -OutputDirectory (New-Stage 'refresh' @('refresh-feedback') $false)
    }
    if('Actions'-in$Stages){
        $path=New-Stage 'actions' @('stash','pending-discard') $false
        & "$PSScriptRoot/benchmark-stash-workflows.ps1" -Repository $Repository -Baseline $Baseline -BaselineHarness $BaselineHarness -Candidate $Candidate -CandidateHarness $CandidateHarness -Samples $Samples -OutputDirectory (Join-Path $path 'stashes')
        & "$PSScriptRoot/benchmark-discard.ps1" -Candidate $Candidate -Samples $Samples -OutputDirectory (Join-Path $path 'discard')
    }
    if('Components'-in$Stages){
        $path=New-Stage 'components' @() $false
        [pscustomobject]@{shared=$shared;candidateOnly=$candidateOnly;retainedSamples=15}|ConvertTo-Json|Set-Content (Join-Path $path 'harness-config.json')
        for($i=0;$i-lt$shared.Count;$i++){
            $test=$shared[$i];$roles=if($i%2-eq0){@('baseline','candidate')}else{@('candidate','baseline')}
            foreach($role in $roles){$exe=if($role-eq'baseline'){$BaselineHarness}else{$CandidateHarness};Harness $exe $test (Join-Path $path "$role-$($test-replace'::','-').txt")}
        }
        foreach($test in $candidateOnly){Harness $CandidateHarness $test (Join-Path $path "candidate-$($test-replace'::','-').txt")}
    }
    Assert-Idle
    & "$PSScriptRoot/summarize-improvements2.ps1" -OutputDirectory $OutputDirectory
    $complete=$true
} finally {
    try {
        if($null-ne$before){
            $after=@(Source-Files);$fixtureAfter=Fixture-Identity
            $sourceSame=(ConvertTo-Json -InputObject @($before) -Compress)-ceq(ConvertTo-Json -InputObject @($after) -Compress)
            $fixtureSame=(ConvertTo-Json -InputObject $fixtureBefore -Compress)-ceq(ConvertTo-Json -InputObject $fixtureAfter -Compress)
            $changed=@($executables|Where-Object{(Get-FileHash -LiteralPath $_.path).Hash-cne$_.sha256})
            [pscustomobject]@{completed=$complete;sourceUnchanged=$sourceSame;fixtureUnchanged=$fixtureSame;changedBinaries=$changed;fixtureAfter=$fixtureAfter;sourceAfter=$after}|ConvertTo-Json -Depth 8|Set-Content (Join-Path $OutputDirectory "integrity-$invocation.json")
            if(!$sourceSame-or!$fixtureSame-or$changed.Count){throw 'Source/binary/fixture changed during measurement; retain raw samples and investigate'}
        }
    }finally{foreach($name in $saved.Keys){if($null-eq$saved[$name]){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}else{[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}}
}
