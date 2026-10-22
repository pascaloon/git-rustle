param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$CandidateHarness,
    [string]$BaselineHarness,
    [string]$Repository,
    [ValidateRange(1,100)][int]$Samples=15,
    [switch]$ReviewDialogCapture,
    [switch]$MinimumWindow,
    [ValidateSet('All','Cleanup')][string]$ComponentSet='All',
    [ValidateSet('Native','Components','Screenshots')][string[]]$Stages=@('Native','Components')
)
$ErrorActionPreference='Stop'
$sourceRoot=(Resolve-Path "$PSScriptRoot/..").Path
. "$PSScriptRoot/benchmark-processes.ps1"
. "$PSScriptRoot/benchmark-screenshot.ps1"
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
$allowed=Join-Path $sourceRoot 'target/bench'
if(!$OutputDirectory.StartsWith($allowed+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Use workspace target/bench/<run>'}
New-Item -ItemType Directory -Path $OutputDirectory -Force|Out-Null
$saved=@{}
$names=@(@(Get-ChildItem Env:|Where-Object Name -Match '^(GIT_|GIT_RUSTLE_)'|ForEach-Object Name)+@('LOCALAPPDATA','APPDATA','TEMP','TMP','GIT_CONFIG_GLOBAL','GIT_CONFIG_SYSTEM','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_COUNT','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_CLEANUP_REVIEW_CAPTURE','GIT_RUSTLE_DUMP','GIT_RUSTLE_BENCH_REPO','GIT_RUSTLE_BENCH_BRANCHES','GIT_RUSTLE_BENCH_WINDOW')|Select-Object -Unique)
foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
function Git([string]$Path,[string[]]$Arguments){$lines=@(& git.exe -C $Path @Arguments 2>&1);if($LASTEXITCODE-ne0){throw "Fixture Git failed: $Arguments $($lines-join' ')"};($lines-join"`n").TrimEnd()}
function Write-FixtureRefs([string]$Path,[string[]]$Commands){
    $start=[Diagnostics.ProcessStartInfo]::new('git.exe')
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.RedirectStandardInput=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($argument in @('-C',$Path,'update-ref','--stdin')){$start.ArgumentList.Add($argument)}
    $process=[Diagnostics.Process]::Start($start)
    try{
        $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
        $process.StandardInput.Write(($Commands-join"`n")+"`n");$process.StandardInput.Close()
        if(!$process.WaitForExit(30000)){$process.Kill($true);throw 'Fixture ref creation timed out'}
        $output=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult()
        if($process.ExitCode-ne0){throw "Fixture ref creation failed: $errorText $output"}
    }finally{$process.Dispose()}
}
function New-Stage([string]$Name){$path=Join-Path $OutputDirectory $Name;if(Test-Path -LiteralPath $path){throw "Retaining existing stage $Name; omit it to resume"};New-Item -ItemType Directory -Path $path|Out-Null;return $path}
function Assert-Idle{Assert-BenchmarkIdle -WorkspaceRoot $sourceRoot -AuditPath (Join-Path $OutputDirectory 'background-processes.json')}
function Read-LiveLog([string]$Path){
    if(!(Test-Path -LiteralPath $Path)){return ''}
    $stream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,([IO.FileShare]::ReadWrite-bor[IO.FileShare]::Delete))
    $reader=[IO.StreamReader]::new($stream)
    try{return $reader.ReadToEnd()}finally{$reader.Dispose()}
}
function Fixture([string]$Root){
    New-Item -ItemType Directory -Path $Root|Out-Null
    $working=Join-Path $Root 'working';$bare=Join-Path $Root 'origin.git'
    New-Item -ItemType Directory -Path $working,$bare|Out-Null
    Git $bare @('init','--bare','-q')|Out-Null;Git $working @('init','-q','-b','main')|Out-Null
    foreach($pair in @(@('user.name','Cleanup Native Owner'),@('user.email','native@example.invalid'),@('core.autocrlf','false'),@('core.hooksPath','disabled-hooks'),@('commit.gpgSign','false'))){Git $working @('config',$pair[0],$pair[1])|Out-Null}
    [IO.File]::WriteAllText((Join-Path $working 'file.txt'),"old`n");Git $working @('add','.')|Out-Null
    $env:GIT_AUTHOR_DATE=$env:GIT_COMMITTER_DATE='2000-01-01T00:00:00Z';Git $working @('commit','-qm','Old local work')|Out-Null
    Git $working @('branch','old/local')|Out-Null
    Remove-Item Env:GIT_AUTHOR_DATE,Env:GIT_COMMITTER_DATE -ErrorAction SilentlyContinue
    [IO.File]::WriteAllText((Join-Path $working 'file.txt'),"current`n");Git $working @('add','.')|Out-Null;Git $working @('commit','-qm','Current matching work')|Out-Null
    Git $working @('branch','fixes/task-123')|Out-Null;Git $working @('branch','master')|Out-Null
    Git $working @('remote','add','origin',$bare)|Out-Null
    Git $working @('push','-qu','origin','main','fixes/task-123','master')|Out-Null
    Git $bare @('symbolic-ref','HEAD','refs/heads/main')|Out-Null
    Git $working @('symbolic-ref','refs/remotes/origin/HEAD','refs/remotes/origin/main')|Out-Null
    # Exercise selection/tree work above the former 256 cap in every section.
    $current=Git $working @('rev-parse','HEAD');$old=Git $working @('rev-parse','refs/heads/old/local')
    $localCommands=[Collections.Generic.List[string]]::new();$serverCommands=[Collections.Generic.List[string]]::new()
    foreach($index in 0..499){
        $name='zz-scroll/{0:d3}'-f$index
        $localCommands.Add("create refs/heads/$name $current");$localCommands.Add("create refs/remotes/origin/$name $current")
        $localCommands.Add("create refs/heads/zz-old/{0:d3} $old"-f$index)
        # Server existence authorizes local-only cleanup even after its tip moved.
        $cached = if($index -eq 0){$old}else{$current}
        $localCommands.Add("create refs/remotes/origin/zz-fetched/{0:d3} $cached"-f$index)
        $serverCommands.Add("create refs/heads/zz-fetched/{0:d3} $current"-f$index)
        $serverCommands.Add("create refs/heads/$name $current")
    }
    # Optional last-copy cleanup is a separate unchecked fourth tree.
    # Forty numbered entries plus the legacy named ref exercise its own scrolling.
    foreach($index in 0..39){$localCommands.Add("create refs/remotes/origin/zz-stale/{0:d3} $old"-f$index)}
    $localCommands.Add("create refs/remotes/origin/zz-stale/missing $old")
    Write-FixtureRefs $working $localCommands.ToArray()
    Write-FixtureRefs $bare $serverCommands.ToArray()
    [IO.File]::WriteAllText((Join-Path $working '.git/git-rustle-cleanup-fixture'),'disposable branch cleanup fixture')
    return $working
}
function Fingerprint([string]$Working){
    [pscustomobject]@{head=(Git $Working @('rev-parse','HEAD'));refs=(Git $Working @('for-each-ref','--format=%(refname) %(objectname)','refs/heads','refs/remotes','refs/tags'));status=(Git $Working @('status','--porcelain=v1'));index=(Git $Working @('ls-files','--stage'));file=(Get-FileHash (Join-Path $Working 'file.txt')).Hash;remote=(Git (Join-Path (Split-Path $Working) 'origin.git') @('for-each-ref','--format=%(refname) %(objectname)'))}
}
function Native([string]$Scenario,[int]$Sample,[string]$Directory,[bool]$Capture){
    Assert-Idle
    $launch=Join-Path $Directory "$Scenario-$Sample";New-Item -ItemType Directory -Path $launch|Out-Null
    $working=Fixture (Join-Path $launch 'fixture');$before=Fingerprint $working
    $env:LOCALAPPDATA=Join-Path $launch 'app-data';$env:APPDATA=Join-Path $launch 'roaming';New-Item -ItemType Directory -Path $env:LOCALAPPDATA,$env:APPDATA|Out-Null
    New-Item -ItemType Directory -Path (Join-Path $env:LOCALAPPDATA 'git-rustle')|Out-Null
    [IO.File]::WriteAllText((Join-Path $env:LOCALAPPDATA 'git-rustle/tools.json'),'{"scanned":true,"tools":[]}')
    if($Capture -and $MinimumWindow){$env:GIT_RUSTLE_BENCH_WINDOW='900x600'}else{Remove-Item Env:GIT_RUSTLE_BENCH_WINDOW -ErrorAction SilentlyContinue}
    $env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_SCENARIO=$Scenario;$env:GIT_RUSTLE_AUTOQUIT=if($Capture){'60000'}else{'100'}
    if($Capture -and $ReviewDialogCapture -and $Scenario -eq 'cleanup-review'){$env:GIT_RUSTLE_CLEANUP_REVIEW_CAPTURE='1'}else{Remove-Item Env:GIT_RUSTLE_CLEANUP_REVIEW_CAPTURE -ErrorAction SilentlyContinue}
    if($Capture){$env:GIT_RUSTLE_TRACE='1'}else{Remove-Item Env:GIT_RUSTLE_TRACE -ErrorAction SilentlyContinue}
    $env:GIT_RUSTLE_DUMP=Join-Path $launch 'state.txt';$log=Join-Path $launch 'app.log';$process=$null
    try{
        $process=Start-BenchmarkNativeProcess -FilePath $Candidate -ArgumentList @('"'+$working+'"','--no-cache') -RedirectStandardError $log
        if($Capture){
            $deadline=[DateTime]::UtcNow.AddSeconds(50)
            do{
                if($process.WaitForExit(0)){throw 'App exited before screenshot checkpoint'}
                if((Read-LiveLog $log).Contains('desktop scenario frame')){break}
                if([DateTime]::UtcNow-gt$deadline){throw "Screenshot checkpoint timed out: $log"}
                Start-Sleep -Milliseconds 75
            }while($true)
            Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $launch 'screenshot.png') -CloseAfterCapture|ConvertTo-Json -Depth 6|Set-Content (Join-Path $launch 'capture-result.json')
        }
        if(!$process.WaitForExit(60000)){throw "Native timeout: $log"};$process.Refresh();if($process.ExitCode-ne0){throw "Native failed: $log"}
        $text=Get-Content $log -Raw
        if($Capture){
            # WM_CLOSE after capture need not execute the autoquit dump path.
            # Both marks are emitted only after action/state/actual-paint checks.
            if(!$text.Contains('branch cleanup verified frame')-or!$text.Contains('desktop scenario frame')){throw "Missing screenshot paint verification: $log"}
        }else{
            $state=Get-Content $env:GIT_RUSTLE_DUMP -Raw
            if($state-notmatch'branch_cleanup_verified=true'){throw "Missing native verification: $log"}
        }
        $after=Fingerprint $working
        [pscustomobject]@{before=$before;after=$after;scenario=$Scenario;sample=$Sample;isolatedDesktop=$process.DesktopName}|ConvertTo-Json -Depth 6|Set-Content (Join-Path $launch 'fixture.json')
        foreach($field in 'head','status','index','file'){if($before.$field-cne$after.$field){throw "Unexpected $field mutation: $log"}}
        if($before.remote-cne$after.remote){throw 'Cleanup must never change server refs'}
        if($before.refs-cne$after.refs){throw 'Cleanup must preserve refs or restore them exactly through Undo'}
        $marks=@{};foreach($m in [regex]::Matches($text,'([\d.]+) ms  ([^\r\n]+)')){if(!$marks.ContainsKey($m.Groups[2].Value)){$marks[$m.Groups[2].Value]=[double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture)}}
        if(!$marks.ContainsKey('branch cleanup requested')-or!$marks.ContainsKey('branch cleanup verified frame')){throw 'Missing measured native marks'}
        [pscustomobject]@{sample=$Sample;scenario=$Scenario;binary='candidate';minimumWindow=[bool]($Capture-and$MinimumWindow);workerMs=$marks['branch cleanup verified frame']-$marks['branch cleanup requested'];deleteMs=if($Scenario-eq'cleanup-delete'){$marks['cleanup delete frame']-$marks['cleanup delete requested']}else{$null};log=$log;capture=$Capture}
    }finally{if($null-ne$process){$process.Dispose()};Assert-Idle}
}
function Harness([string]$Exe,[string]$Test,[string]$Log){Assert-Idle;& $Exe $Test --ignored --exact --nocapture 2>&1|Tee-Object -FilePath $Log;if($LASTEXITCODE-ne0){throw "Harness failed: $Log"};$text=Get-Content $Log -Raw;if($text-notmatch'running 1 test'-or$text-notmatch'test result: ok\. 1 passed;'){throw "Expected one passing harness: $Log"};Assert-Idle}
try{
    foreach($name in @($names|Where-Object{$_-match'^(GIT_|GIT_RUSTLE_)'})){Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue}
    $env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_CONFIG_GLOBAL=Join-Path $OutputDirectory 'empty-config';[IO.File]::WriteAllText($env:GIT_CONFIG_GLOBAL,'')
    $env:GIT_CONFIG_COUNT='3';$env:GIT_CONFIG_KEY_0='safe.directory';$env:GIT_CONFIG_VALUE_0=$sourceRoot.Replace('\','/');$env:GIT_CONFIG_KEY_1='safe.directory';$env:GIT_CONFIG_VALUE_1=($OutputDirectory+'/*').Replace('\','/')
    $env:GIT_CONFIG_KEY_2='safe.directory';$env:GIT_CONFIG_VALUE_2=if($Repository){(Resolve-Path $Repository).Path.Replace('\','/')}else{$sourceRoot.Replace('\','/')}
    $env:TEMP=$env:TMP=Join-Path $OutputDirectory 'temp';New-Item -ItemType Directory -Path $env:TEMP -Force|Out-Null
    $sourceFiles=@(Get-ChildItem src,scripts -Recurse -File|Sort-Object FullName|ForEach-Object{[pscustomobject]@{path=$_.FullName;sha256=(Get-FileHash $_.FullName).Hash}})
    $hash=(Get-FileHash $Candidate).Hash
    [pscustomobject]@{candidate=$Candidate;sha256=$hash;samples=$Samples;stages=$Stages;componentSet=$ComponentSet;minimumWindow=[bool]$MinimumWindow;source=$sourceFiles}|ConvertTo-Json -Depth 6|Set-Content (Join-Path $OutputDirectory "provenance-$([Guid]::NewGuid().ToString('N')).json")
    if('Native'-in$Stages){$directory=New-Stage 'native';$rows=@();foreach($scenario in 'branches-hierarchy','cleanup-review','cleanup-delete'){Native $scenario -1 $directory $false|Out-Null;for($sample=0;$sample-lt$Samples;$sample++){$rows+=Native $scenario $sample $directory $false;$rows|ConvertTo-Json -Depth 6|Set-Content (Join-Path $directory 'samples.json')}}}
    if('Components'-in$Stages){
        if(!$CandidateHarness){throw 'Components requires CandidateHarness'};$CandidateHarness=(Resolve-Path $CandidateHarness).Path;$directory=New-Stage 'components'
        $env:GIT_RUSTLE_BENCH_BRANCHES='1000';if($Repository){$env:GIT_RUSTLE_BENCH_REPO=(Resolve-Path $Repository).Path}
        if($BaselineHarness){$BaselineHarness=(Resolve-Path $BaselineHarness).Path}
        if($BaselineHarness-and$ComponentSet-eq'All'){foreach($test in 'branches::tests::benchmark_header_sort','branches::tests::benchmark_owner_filter','branches::tests::benchmark_browser'){
            if($test-eq'branches::tests::benchmark_browser'-and!$Repository){throw 'Shared browser worker needs Repository'}
            foreach($role in 'baseline','candidate'){$exe=if($role-eq'baseline'){$BaselineHarness}else{$CandidateHarness};Harness $exe $test (Join-Path $directory "$role-$($test-replace'::','-').txt")}
        }}
        if($ComponentSet-eq'All'){Harness $CandidateHarness 'branches::hierarchy_tests::benchmark_branch_projection' (Join-Path $directory 'candidate-branches-hierarchy_tests-benchmark_branch_projection.txt')}
        # Existing cleanup harnesses retain identical fixtures/dimensions across
        # baseline and candidate; optional orphan deletion is covered natively.
        foreach($test in 'git::operations::cleanup::tests::benchmark_cleanup','git::operations::cleanup::tests::benchmark_cleanup_remote','git::operations::cleanup::tests::benchmark_cleanup_large','git::operations::cleanup::tests::benchmark_cleanup_matching_large'){
            $roles=if($BaselineHarness){@('baseline','candidate')}else{@('candidate')}
            foreach($role in $roles){$exe=if($role-eq'baseline'){$BaselineHarness}else{$CandidateHarness};Harness $exe $test (Join-Path $directory "$role-$($test-replace'::','-').txt")}
        }
    }
    if('Screenshots'-in$Stages){$directory=New-Stage 'screenshots';foreach($scenario in 'branches-hierarchy','cleanup-review'){Native $scenario 0 $directory $true|ConvertTo-Json -Depth 5|Set-Content (Join-Path $directory "$scenario.json")}}
    $after=@(Get-ChildItem src,scripts -Recurse -File|Sort-Object FullName|ForEach-Object{[pscustomobject]@{path=$_.FullName;sha256=(Get-FileHash $_.FullName).Hash}})
    if((ConvertTo-Json -InputObject $sourceFiles -Compress)-cne(ConvertTo-Json -InputObject $after -Compress)-or(Get-FileHash $Candidate).Hash-cne$hash){throw 'Source or binary changed during run'}
    $limitations='Native features are candidate-only absolute costs. Fixtures use local bare remotes; every scenario verifies unchanged server refs. Missing-server cached refs are optional and initially unchecked; explicitly selected orphan cleanup is local-only. Mutation scenarios verify deletion and restore exact local/cached refs through Undo, Redo and final Undo. Native request-to-verified-frame intervals include recovery/state/paint scheduling, not just worker CPU. Minimum-window captures use900x600 diagnostic logical bounds and never enter timing samples. Repack retains recovery/unreachable objects; reported storage is Git-reported KiB, not physical allocated disk bytes. Screenshot launches are excluded from timing. Shared component controls retain baseline/candidate raw logs. These fixtures do not establish network behavior, physical input or display scanout.'
    $report=@('# Branch and cleanup measurements','','| Scenario | Samples | Median ms | p95 ms |','|---|---:|---:|---:|')
    $nativeSamples=Join-Path $OutputDirectory 'native/samples.json'
    if(Test-Path -LiteralPath $nativeSamples){
        foreach($group in @(Get-Content $nativeSamples -Raw|ConvertFrom-Json|Group-Object scenario)){
            foreach($metric in @('workerMs','deleteMs')){
                $values=@($group.Group|Where-Object{$null-ne$_.$metric}|ForEach-Object{[double]$_.$metric}|Sort-Object)
                if($values.Count){$middle=[int][Math]::Floor($values.Count/2);$median=if($values.Count%2){$values[$middle]}else{($values[$middle-1]+$values[$middle])/2};$p95=$values[[Math]::Ceiling(.95*$values.Count)-1];$report+='| {0} {1} | {2} | {3:F3} | {4:F3} |'-f$group.Name,$metric,$values.Count,$median,$p95}
            }
        }
    }
    $report+=@('','Request-to-verified-paint intervals include asynchronous work, action scheduling and actual entry/modal paint assertions. New workflows have no equivalent baseline action. Component logs contain their own tagged raw samples; compare only identical tags, fields and fixture scale.','',$limitations)
    $report|Set-Content (Join-Path $OutputDirectory 'report.md')
    $limitations|Set-Content (Join-Path $OutputDirectory 'limitations.md')
}finally{
    foreach($name in @('GIT_CONFIG_KEY_0','GIT_CONFIG_VALUE_0','GIT_CONFIG_KEY_1','GIT_CONFIG_VALUE_1','GIT_CONFIG_KEY_2','GIT_CONFIG_VALUE_2')){if(!$saved.ContainsKey($name)){Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue}}
    foreach($name in $saved.Keys){if($null-eq$saved[$name]){Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue}else{[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
}
