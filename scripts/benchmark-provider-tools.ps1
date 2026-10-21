param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$CandidateHarness,
    [string]$BaselineHarness,
    [ValidateSet('provider-scroll','provider-menu','tool-icons','tool-icons-diff')][string[]]$Scenarios = @('provider-scroll','provider-menu','tool-icons','tool-icons-diff'),
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateRange(5,60)][int]$TimeoutSeconds = 30,
    [switch]$SkipNative,
    [switch]$SkipScreenshots,
    [switch]$SkipComponents
)
# No desktop input or screen capture. Every native process uses an owned private
# desktop. Editors are resource fixtures only and are never launched.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/benchmark-screenshot.ps1"
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$allowed = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../target/bench')) + [IO.Path]::DirectorySeparatorChar
if (!$OutputDirectory.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase)) { throw 'Use an output directory under this workspace target/bench' }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Retaining prior evidence; choose a new output directory' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$saved = @{}
$names = @('LOCALAPPDATA','APPDATA','GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_COUNT','GIT_TERMINAL_PROMPT','GIT_OPTIONAL_LOCKS','GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES','GIT_RUSTLE_TOOL_DISCOVERY_ROOTS','GIT_RUSTLE_ICON_BENCH_EXE')
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
function Assert-Idle {
    if (@(Get-Process cargo,rustc,link,cl -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: competing build detected' }
}
function Fixture-Git([string]$Path,[string[]]$Arguments) {
    $result = @(& git -C $Path -c core.hooksPath= -c commit.gpgsign=false @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "Fixture Git failed: $Arguments $result" }
    ($result -join "`n").TrimEnd()
}
function Fixture-State([string]$Path) {
    [ordered]@{
        head = (Fixture-Git $Path @('rev-parse','HEAD'))
        refs = (Fixture-Git $Path @('show-ref'))
        status = (Fixture-Git $Path @('status','--porcelain=v1'))
        stash = (Fixture-Git $Path @('stash','list','--format=%H'))
        index = (Get-FileHash -LiteralPath (Join-Path $Path '.git/index')).Hash
        file = (Get-FileHash -LiteralPath (Join-Path $Path 'fixture.txt')).Hash
    } | ConvertTo-Json -Compress
}
function New-Fixture([string]$Directory) {
    $repository = Join-Path $Directory 'repository'
    New-Item -ItemType Directory -Path $repository | Out-Null
    Fixture-Git $repository @('init','-b','main') | Out-Null
    Fixture-Git $repository @('config','user.name','Isolated provider fixture') | Out-Null
    Fixture-Git $repository @('config','user.email','fixture@example.invalid') | Out-Null
    foreach ($n in 1..8) {
        $env:GIT_AUTHOR_DATE = $env:GIT_COMMITTER_DATE = "2026-10-19T00:00:0${n}Z"
        [IO.File]::WriteAllText((Join-Path $repository 'fixture.txt'),"Isolated provider tools fixture revision $n`n")
        Fixture-Git $repository @('add','fixture.txt') | Out-Null
        Fixture-Git $repository @('commit','-m',"Fixture revision $n") | Out-Null
    }
    $env:LOCALAPPDATA = Join-Path $Directory 'appdata'
    $env:APPDATA = Join-Path $Directory 'roaming'
    $settings = Join-Path $env:LOCALAPPDATA 'git-rustle'
    New-Item -ItemType Directory -Path $settings,$env:APPDATA | Out-Null
    $entries = @(
        @{id='resource';name='Fixture application icon';executable=$Baseline;preset='Custom';diff_arguments=@('{left}','{right}');merge_arguments=@('{left}','{base}','{right}','{output}')},
        @{id='fallback';name='Unavailable fixture tool';executable=(Join-Path $Directory 'missing-tool.exe');preset='Custom';diff_arguments=@('{left}','{right}');merge_arguments=@('{left}','{base}','{right}','{output}')}
    )
    @{scanned=$true;tools=$entries} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $settings 'tools.json')
    $repository
}
function Run-Native([string]$Executable,[string]$Label,[string]$Scenario,[int]$Sample,[bool]$Capture) {
    Assert-Idle
    $group = if ($Capture) { 'screenshots' } else { 'native' }
    $directory = Join-Path $OutputDirectory "$group/$Label-$Scenario-$Sample"
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    $repository = New-Fixture $directory
    $before = Fixture-State $repository
    $before | Set-Content -LiteralPath (Join-Path $directory 'fixture-before.json')
    $env:GIT_RUSTLE_TIMING='1'; $env:GIT_RUSTLE_TRACE='1'; $env:GIT_RUSTLE_SCENARIO=$Scenario
    $env:GIT_RUSTLE_AUTOQUIT = if ($Capture) { '600000' } else { '1200' }
    $env:GIT_RUSTLE_DUMP = Join-Path $directory 'state.txt'
    $env:GIT_RUSTLE_FRAME_SAMPLES = Join-Path $directory 'frames.json'
    $log = Join-Path $directory 'app.log'
    $process = $null
    $captureMetadata = $null
    $clock = [Diagnostics.Stopwatch]::StartNew()
    try {
        $process = Start-BenchmarkNativeProcess -FilePath $Executable -ArgumentList @(('"'+$repository+'"'),'--no-cache') -RedirectStandardError $log
        if ($Capture) {
            do {
                if ($process.WaitForExit(0)) { throw 'App exited before screenshot readiness' }
                $text = Get-Content -LiteralPath $log -Raw
                if ($text -match '\bdesktop scenario frame\b') { break }
                if ($clock.Elapsed.TotalSeconds -gt $TimeoutSeconds) { throw "Scenario paint timed out: $directory" }
                Start-Sleep -Milliseconds 50
            } while ($true)
            $captureMetadata = Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $directory 'screenshot.png') -CloseAfterCapture
        }
        if (!$process.WaitForExit($TimeoutSeconds*1000)) {
            # A failure diagnostic is outside every accepted timing sample.
            try { Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $directory 'timeout.png') -CloseAfterCapture | Out-Null } catch { $_ | Out-String | Set-Content (Join-Path $directory 'timeout-capture-error.txt') }
            throw "Native scenario timed out: $directory"
        }
        $clock.Stop()
        if ($process.ExitCode -ne 0) { throw "Native scenario failed: $directory" }
    } finally { if ($null -ne $process) { $process.Dispose() } }
    Assert-Idle
    $after = Fixture-State $repository
    $after | Set-Content -LiteralPath (Join-Path $directory 'fixture-after.json')
    if ($before -cne $after) { throw "Scenario changed its fixture: $directory" }
    $text = Get-Content -LiteralPath $log -Raw
    $marks = @{}
    foreach ($match in [regex]::Matches($text,'(?m)^\s*([\d.]+) ms\s+(.+)\r?$')) { $marks[$match.Groups[2].Value.Trim()] = [double]::Parse($match.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    $firstFrame = @($marks.Keys | Where-Object { $_ -like 'first frame (*' } | Select-Object -First 1)
    if (!$firstFrame.Count -or !$marks.ContainsKey('desktop scenario frame')) { throw "Missing first/scenario native paint marks: $directory" }
    $frames = @()
    if (!$Capture) {
        $state = Get-Content -LiteralPath $env:GIT_RUSTLE_DUMP -Raw
        if ($Scenario -like 'provider-*') {
            if ($state -notmatch 'improvement_verified=true') { throw 'Provider scenario behavioral checks did not complete' }
            if ($Label -eq 'candidate' -and $state -notmatch 'provider_pipeline_paint_mask=31') { throw 'All five pipeline icon paints were not observed' }
        } elseif ($state -notmatch 'tool_icons_actual=[1-9]\d* tool_icons_fallback=[1-9]\d* tool_icons_painted=[2-9]\d*') { throw 'Actual and fallback tool icons were not decoded and painted' }
        if ($Scenario -eq 'provider-scroll') {
            $frames = @(Get-Content -LiteralPath $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json)
            if ($frames.Count -ne 120) { throw 'Expected 120 measured scroll frames' }
        }
    }
    $requested=if($marks.ContainsKey('desktop improvement requested')){$marks['desktop improvement requested']}elseif($marks.ContainsKey('desktop scenario requested')){$marks['desktop scenario requested']}else{$marks[$firstFrame[0]]}
    [pscustomobject]@{binary=$Label;scenario=$Scenario;sample=$Sample;screenshot=$Capture;wall_ms=$clock.Elapsed.TotalMilliseconds;first_frame_ms=$marks[$firstFrame[0]];scenario_frame_ms=$marks['desktop scenario frame'];operation_ms=($marks['desktop scenario frame']-$requested);frames_ms=$frames;cleanupVerified=$process.WaitForExit(0);fixtureUnchanged=$true;capture=$captureMetadata;directory=$directory}
}
function Run-Harness([string]$Executable,[string]$Filter,[string]$Name,[bool]$Ignored) {
    if (!$Executable) { return }
    Assert-Idle
    $stdout=Join-Path $OutputDirectory "$Name.stdout.log"; $stderr=Join-Path $OutputDirectory "$Name.stderr.log"
    $arguments=@($Filter,'--nocapture'); if ($Ignored) {$arguments+=@('--ignored','--exact')}
    $process=Start-Process -FilePath (Resolve-Path -LiteralPath $Executable).Path -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
    try {
        if (!$process.WaitForExit(60000)) { throw "Component timed out: $Name" }
        $process.Refresh()
        if ($process.ExitCode -ne 0 -or (Get-Content -LiteralPath $stdout -Raw) -notmatch 'test result: ok\. [1-9][0-9]* passed;') { throw "Component failed: $Name" }
        $diagnostics=(Get-Content -LiteralPath $stdout -Raw)+(Get-Content -LiteralPath $stderr -Raw)
        if($diagnostics-match'UNSUPPORTED:') {
            $unsupportedPath=Join-Path $OutputDirectory 'unsupported-integrations.json'
            $unsupported=if(Test-Path -LiteralPath $unsupportedPath){@(Get-Content -LiteralPath $unsupportedPath -Raw|ConvertFrom-Json)}else{@()}
            $unsupported+=@{name=$Name;filter=$Filter;reason='Windows Credential Manager persistence unavailable in this logon session; pure selection/error tests run, affected Windows integration returns explicitly unsupported';log=$stderr}
            ConvertTo-Json -InputObject @($unsupported) -Depth 4 | Set-Content -LiteralPath $unsupportedPath
        }
    } finally {if (!$process.HasExited) {$process.Kill($true);$process.WaitForExit()};$process.Dispose()}
    Assert-Idle
}
try {
    foreach($name in $names) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    $env:GIT_CONFIG_NOSYSTEM='1';$env:GIT_CONFIG_COUNT='0';$env:GIT_TERMINAL_PROMPT='0';$env:GIT_OPTIONAL_LOCKS='0'
    $env:GIT_CONFIG_GLOBAL=Join-Path $OutputDirectory 'empty-gitconfig';[IO.File]::WriteAllText($env:GIT_CONFIG_GLOBAL,'')
    $env:GIT_RUSTLE_ICON_BENCH_EXE=$Baseline
    @{candidate=$Candidate;candidate_sha256=(Get-FileHash $Candidate).Hash;baseline=$Baseline;baseline_sha256=(Get-FileHash $Baseline).Hash;samples=$Samples;warmups=1;recordedUtc=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'provenance.json')
    $results=@()
    if (!$SkipNative) {
        foreach($scenario in $Scenarios) {
            foreach($sample in -1..($Samples-1)) {
                $labels=if($scenario-like'tool-icons*') {@('candidate')} elseif($sample%2-eq0) {@('baseline','candidate')} else {@('candidate','baseline')}
                foreach($label in $labels) {
                    $executable=if($label-eq'baseline'){$Baseline}else{$Candidate}
                    $results+=Run-Native $executable $label $scenario $sample $false
                    $results | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutputDirectory 'native-samples.json')
                }
            }
        }
    }
    if (!$SkipComponents) {
        $env:LOCALAPPDATA=Join-Path $OutputDirectory 'component-appdata'
        New-Item -ItemType Directory -Path $env:LOCALAPPDATA | Out-Null
        foreach($filter in @('remote::requests::pipelines::tests','provider_settings::tests','credentials::tests','desktop::provider::tests','external_tools::icons::tests')) {Run-Harness $CandidateHarness $filter ('tests-'+$filter.Replace('::','-')) $false}
        foreach($label in @('baseline','candidate')) {
            $harness=if($label-eq'baseline'){$BaselineHarness}else{$CandidateHarness}
            Run-Harness $harness 'desktop::provider::tests::benchmark_provider_lookup' "$label-provider-lookup" $true
        }
        Run-Harness $CandidateHarness 'remote::requests::pipelines::tests::benchmark_pipeline_snapshot' 'candidate-pipeline-snapshot' $true
        Run-Harness $CandidateHarness 'external_tools::icons::tests::benchmark_executable_icons' 'candidate-tool-icons' $true
    }
    if (!$SkipScreenshots) {
        $captures=@(foreach($scenario in $Scenarios) {Run-Native $Candidate 'candidate' $scenario 0 $true})
        $captures | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutputDirectory 'screenshots.json')
    }
    $report=@('# Provider and executable icon validation','','Raw launch samples include one excluded warmup (sample -1) and retained samples. Baseline/candidate order alternates for shared provider scenarios. Tool-icons and pipeline snapshot/extraction component operations are candidate-only because preserved baseline has no equivalent feature. Screenshots run separately after timing samples.','','| Binary / scenario | Metric | Samples | Median ms | p95 ms |','| --- | --- | ---: | ---: | ---: |')
    foreach($group in @($results | Where-Object {$_.sample-ge0} | Group-Object binary,scenario)) {
        foreach($metric in @('wall_ms','first_frame_ms','operation_ms','frames_ms')) {
            $values=@($group.Group | ForEach-Object { $_.$metric } | Sort-Object)
            if(!$values.Count){continue}
            $middle=[int][Math]::Floor($values.Count/2)
            $median=if($values.Count%2){$values[$middle]}else{($values[$middle-1]+$values[$middle])/2}
            $p95=$values[[int][Math]::Ceiling(.95*$values.Count)-1]
            $report+='| {0} | {1} | {2} | {3:F3} | {4:F3} |' -f $group.Name,$metric,$values.Count,$median,$p95
        }
    }
    foreach($log in @(Get-ChildItem -LiteralPath $OutputDirectory -Filter '*.stdout.log')) {
        $rows=@(foreach($line in Get-Content -LiteralPath $log.FullName) {if($line.StartsWith('{')) {try {$row=$line|ConvertFrom-Json;if($row.PSObject.Properties.Name-contains'sample'){$row}}catch{}}})
        foreach($metric in @('worker_ms','cache_ms','extraction_ms','cache_hit_ms')) {
            $values=@($rows | Where-Object {$_.PSObject.Properties.Name-contains$metric} | ForEach-Object {$_.$metric} | Sort-Object)
            if(!$values.Count){continue}
            $middle=[int][Math]::Floor($values.Count/2)
            $median=if($values.Count%2){$values[$middle]}else{($values[$middle-1]+$values[$middle])/2}
            $report+='| {0} | {1} | {2} | {3:F6} | {4:F6} |' -f $log.BaseName,$metric,$values.Count,$median,$values[[int][Math]::Ceiling(.95*$values.Count)-1]
        }
        $text=Get-Content -LiteralPath $log.FullName -Raw
        if($text-match'provider_40_visible_labels_index_ms_per_frame=(\[[^\r\n]*\])') {
            $values=@($Matches[1]|ConvertFrom-Json|Sort-Object)
            $middle=[int][Math]::Floor($values.Count/2)
            $report+='| {0} | 40 visible provider lookups / frame | {1} | {2:F6} | {3:F6} |' -f $log.BaseName,$values.Count,$values[$middle],$values[[int][Math]::Ceiling(.95*$values.Count)-1]
        }
    }
    $report+=@('','Nearest-rank p95. Native wall_ms includes launch/startup through graceful exit; first_frame_ms and scenario_frame_ms use the application trace clock. operation_ms measures scenario action request through verified scenario frame (including 120 scrolling frames for provider-scroll). Frame arrays contain 120 native GPUI callback intervals per scrolling launch; those intervals are correlated. Component logs define narrower extraction, cache and loopback-worker timings; do not compare different boundaries. Fixtures have no real provider remotes, editors are never launched, all owned native processes/private desktops are disposed, and repository postconditions are checked. Screenshot appearance still requires PNG inspection. Physical input, interactive desktop composition, display scanout and real provider latency are excluded.')
    if(Test-Path -LiteralPath (Join-Path $OutputDirectory 'unsupported-integrations.json')) {$report+=@('','Windows Credential Manager persistence was unsupported for some integration tests in this logon session. Those real-store operations were not verified; independent pure/mock credential tests still ran. See unsupported-integrations.json and retained stderr logs for exact affected filters.')}
    $report | Set-Content (Join-Path $OutputDirectory 'report.md')
} finally {foreach($name in $saved.Keys) {[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
