param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15
)
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
. "$PSScriptRoot/benchmark-processes.ps1"
foreach($name in 'Repository','Baseline','BaselineHarness','CandidateHarness','Candidate'){
    Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path
}
$allowed = [IO.Path]::GetFullPath((Join-Path $sourceRoot 'target/bench'))
$outputFull = [IO.Path]::GetFullPath($OutputDirectory)
if(!$outputFull.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Results must stay under workspace target/bench/<run>'}
if(Test-Path -LiteralPath $outputFull){throw 'Retaining existing results; choose a new output directory'}
New-Item -ItemType Directory -Path $outputFull|Out-Null
$OutputDirectory = $outputFull
$fixtureRoot = Join-Path $OutputDirectory 'fixtures'
New-Item -ItemType Directory -Path $fixtureRoot|Out-Null
[IO.File]::WriteAllText((Join-Path $fixtureRoot '.git-rustle-benchmark-fixtures'),'disposable git-rustle todo fixtures')
$saved=@{}
foreach($name in 'LOCALAPPDATA','TEMP','TMP','GIT_CONFIG_COUNT','GIT_OPTIONAL_LOCKS','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES'){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
function Assert-Idle { Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
function Sources {
    @(Get-ChildItem -LiteralPath (Join-Path $sourceRoot 'src'),(Join-Path $sourceRoot 'scripts') -Recurse -File|Sort-Object FullName|ForEach-Object{[pscustomobject]@{path=$_.FullName;sha256=(Get-FileHash -LiteralPath $_.FullName).Hash}})|ConvertTo-Json -Depth 4 -Compress
}
function Harness([string]$Exe,[string]$Test,[string]$Log){
    Assert-Idle
    & $Exe $Test --ignored --exact --nocapture 2>&1|Tee-Object -FilePath $Log
    if($LASTEXITCODE-ne0){throw "Harness failed: $Log"}
    $text=Get-Content -LiteralPath $Log -Raw
    if($text-notmatch'running 1 test'-or$text-notmatch'test result: ok\. 1 passed;'){throw "Expected exactly one passing test: $Log"}
    Assert-Idle
}
function Stats($Values){
    $sorted=@($Values|Sort-Object)
    [pscustomobject]@{n=$sorted.Count;median=$sorted[[int][Math]::Floor(($sorted.Count-1)*0.5)];p95=$sorted[[int][Math]::Ceiling(($sorted.Count-1)*0.95)]}
}
try {
    Assert-Idle
    $before=Sources
    $before|Set-Content (Join-Path $OutputDirectory 'source-before.json')
    $binaries=@($Baseline,$BaselineHarness,$Candidate,$CandidateHarness)|ForEach-Object{[pscustomobject]@{path=$_;sha256=(Get-FileHash -LiteralPath $_).Hash}}
    $binaries|ConvertTo-Json|Set-Content (Join-Path $OutputDirectory 'binaries.json')
    foreach($name in 'LOCALAPPDATA','TEMP','TMP'){
        $path=Join-Path $OutputDirectory $name
        New-Item -ItemType Directory -Path $path|Out-Null
        [Environment]::SetEnvironmentVariable($name,$path,'Process')
    }
    foreach($name in 'GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_OBJECT_DIRECTORY','GIT_ALTERNATE_OBJECT_DIRECTORIES'){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}
    $configCount=0
    if($saved.GIT_CONFIG_COUNT){$configCount=[int]$saved.GIT_CONFIG_COUNT}
    foreach($path in @($sourceRoot,$Repository,(Join-Path $fixtureRoot '*'))){
        foreach($part in @(@('KEY','safe.directory'),@('VALUE',$path.Replace('\','/')))){
            $name="GIT_CONFIG_$($part[0])_$configCount"
            $saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')
            [Environment]::SetEnvironmentVariable($name,$part[1],'Process')
        }
        $configCount++
    }
    $env:GIT_CONFIG_COUNT="$configCount"; $env:GIT_OPTIONAL_LOCKS='0'
    & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -Samples $Samples -Scenarios @('stash-panel','stash-apply','stash-delete') -DisposableFixtureRoot $fixtureRoot -RejectCompetingBuilds -OutputDirectory (Join-Path $OutputDirectory 'shared')
    & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate -Samples $Samples -Scenarios @('stash-create') -DisposableFixtureRoot $fixtureRoot -RejectCompetingBuilds -OutputDirectory (Join-Path $OutputDirectory 'create')
    Harness $BaselineHarness 'git::operations::tests::benchmark_stashes' (Join-Path $OutputDirectory 'baseline-stashes.txt')
    Harness $CandidateHarness 'git::operations::tests::benchmark_stashes' (Join-Path $OutputDirectory 'candidate-stashes.txt')
    Harness $CandidateHarness 'git::operations::tests::benchmark_stash_create' (Join-Path $OutputDirectory 'candidate-create.txt')
    $rows=@(Get-Content (Join-Path $OutputDirectory 'shared/samples.json') -Raw|ConvertFrom-Json)
    $report=@('# Stash workflow measurements','','| Scenario | Baseline median/p95 ms | Candidate median/p95 ms | Median delta | p95 delta | Gate |','|---|---:|---:|---:|---:|---|')
    foreach($scenario in 'stash-panel','stash-apply','stash-delete'){
        $a=Stats @($rows|Where-Object {$_.binary-eq'baseline'-and$_.scenario-eq$scenario}|ForEach-Object workerMs)
        $b=Stats @($rows|Where-Object {$_.binary-eq'candidate'-and$_.scenario-eq$scenario}|ForEach-Object workerMs)
        if($a.n-ne$Samples-or$b.n-ne$Samples){throw 'Incomplete native samples'}
        $delta=$b.median-$a.median; $tail=$b.p95-$a.p95
        $gate=if(($delta-gt1-and$delta-gt($a.median*.05))-or($tail-gt1-and$tail-gt($a.p95*.05))){'CROSSED'}else{'within joint gate'}
        $report+='| {0} | {1:F3}/{2:F3} | {3:F3}/{4:F3} | {5:+0.000;-0.000;0} ms ({6:+0.0;-0.0;0}%) | {7:+0.000;-0.000;0} ms ({8:+0.0;-0.0;0}%) | {9} |'-f $scenario,$a.median,$a.p95,$b.median,$b.p95,$delta,($delta/[Math]::Max(.001,$a.median)*100),$tail,($tail/[Math]::Max(.001,$a.p95)*100),$gate
    }
    $intervals=@()
    foreach($sample in 0..($Samples-1)){
        $text=Get-Content (Join-Path $OutputDirectory "create/candidate-stash-create-$sample.txt") -Raw
        $marks=@{};foreach($m in [regex]::Matches($text,'([\d.]+) ms  ([^\r\n]+)')){$marks[$m.Groups[2].Value]=[double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture)}
        foreach($op in 'create','external add','external drop','global refresh'){
            $start="stash $op requested"; $end="stash $op frame"
            if(!$marks.ContainsKey($start)-or!$marks.ContainsKey($end)){throw "Missing $op marks in sample $sample"}
            $intervals+=[pscustomobject]@{sample=$sample;operation=$op;ms=$marks[$end]-$marks[$start]}
        }
    }
    $intervals|ConvertTo-Json|Set-Content (Join-Path $OutputDirectory 'create-intervals.json')
    $report+=@('','| Candidate-only interval | Median ms | p95 ms |','|---|---:|---:|')
    foreach($op in 'create','external add','external drop','global refresh'){$s=Stats @($intervals|Where-Object operation -EQ $op|ForEach-Object ms);$report+='| {0} | {1:F3} | {2:F3} |'-f $op,$s.median,$s.p95}
    $after=Sources; $after|Set-Content (Join-Path $OutputDirectory 'source-after.json')
    if($before-cne$after){throw 'Source changed during measurement; retain raw samples but invalidate run'}
    foreach($binary in $binaries){if((Get-FileHash -LiteralPath $binary.path).Hash-cne$binary.sha256){throw 'Binary changed during measurement'}}
    $report+=@('','Creation exercises optional named input and saves tracked and untracked changes. The final candidate-only global refresh check queues another explicit Refresh during an active stash list and verifies two newer list deliveries plus actual entry paint; the bundled stash-create duration includes this check. External unnamed creation and deletion use a worker and are observed through the existing two-second status cadence; their times include scheduling/watcher cadence. Shared scenarios each discard one warmup and retain the requested sample count. Worker harnesses retain fifteen samples after their own warmup. Raw worker logs must be reviewed alongside native comparisons. Small disposable local repositories do not establish all repository scales, physical pointer input/display scanout, or employer-network performance. Pair this suite with fresh startup and pending-worker comparisons because shared status now performs four bounded metadata probes.')
    $report|Set-Content (Join-Path $OutputDirectory 'report.md')
} finally {foreach($name in $saved.Keys){if($null-eq$saved[$name]){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}else{[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}}
