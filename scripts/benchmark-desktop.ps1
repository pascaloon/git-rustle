param(
    [Parameter(Mandatory)][string]$Repository,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [string]$Baseline,
    [string]$ImageCacheSeed,
    [string]$DisposableFixtureRoot,
    [string]$ToolsDiscoveryFixtureRoot,
    [switch]$RequireSyntax,
    [switch]$RequireSelectedPath,
    [ValidateRange(0,200000)][int]$ExpectedSyntaxSpans = 0,
    [switch]$RejectCompetingBuilds,
    [int]$Samples = 15,
    [string[]]$Scenarios = @('scroll','branches','diff','search','settings'),
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\desktop"
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/benchmark-processes.ps1"
. "$PSScriptRoot/benchmark-native-process.ps1"
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
if ($Baseline) { $Baseline = (Resolve-Path -LiteralPath $Baseline).Path }
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
$names = @('GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES','LOCALAPPDATA','GIT_RUSTLE_TOOL_DISCOVERY_ROOTS')
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
function Fixture-Git([string]$Path,[string[]]$Arguments) {
    $result = @(& git -c "safe.directory=$($Path.Replace('\','/'))" -c core.hooksPath= -c core.autocrlf=false -C $Path @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "Disposable fixture Git failed ($Arguments): $($result -join ' ')" }
    return ($result -join "`n").TrimEnd()
}
function Fixture-State([string]$Path) {
    $files = @(Get-ChildItem -LiteralPath $Path -Recurse -File | Where-Object { !$_.FullName.StartsWith((Join-Path $Path '.git')+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase) } | Sort-Object FullName | ForEach-Object {
        [pscustomobject]@{path=[IO.Path]::GetRelativePath($Path,$_.FullName);sha256=(Get-FileHash -LiteralPath $_.FullName).Hash}
    })
    [pscustomobject]@{head=(Fixture-Git $Path @('rev-parse','HEAD'));branch=(Fixture-Git $Path @('symbolic-ref','HEAD'))
        refs=(Fixture-Git $Path @('for-each-ref','--format=%(refname) %(objectname)','refs/heads','refs/remotes','refs/tags'))
        status=(Fixture-Git $Path @('status','--porcelain=v1'));stashes=@((Fixture-Git $Path @('stash','list','--format=%H')) -split "`n" | Where-Object { $_ })
        index=(Get-FileHash -LiteralPath (Join-Path $Path '.git/index')).Hash;files=$files}
}
function New-DisposableFixture([string]$Scenario,[string]$Label,[int]$Sample) {
    if (!$DisposableFixtureRoot) { throw "$Scenario requires a marker-guarded DisposableFixtureRoot" }
    $root = (Resolve-Path -LiteralPath $DisposableFixtureRoot).Path
    $marker = Join-Path $root '.git-rustle-benchmark-fixtures'
    if (!(Test-Path -LiteralPath $marker) -or [IO.File]::ReadAllText($marker) -cne 'disposable git-rustle todo fixtures') { throw 'Missing disposable todo fixture root marker' }
    $path = Join-Path $root "$Label-$Scenario-$Sample"
    if (Test-Path -LiteralPath $path) { throw "Retaining existing disposable fixture: $path" }
    New-Item -ItemType Directory -Path $path | Out-Null
    $dates = @{};foreach($name in 'GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_OPTIONAL_LOCKS'){ $dates[$name]=[Environment]::GetEnvironmentVariable($name,'Process') }
    try {
        $env:GIT_OPTIONAL_LOCKS='0'
        Fixture-Git $path @('init','-b','main') | Out-Null
        Fixture-Git $path @('config','user.name','Benchmark') | Out-Null
        Fixture-Git $path @('config','user.email','benchmark@example.invalid') | Out-Null
        Fixture-Git $path @('config','commit.gpgsign','false') | Out-Null
        Fixture-Git $path @('config','core.autocrlf','false') | Out-Null
        Fixture-Git $path @('config','core.hooksPath','disabled-test-hooks') | Out-Null
        [IO.File]::WriteAllText((Join-Path $path '.git/git-rustle-todos-fixture'),'disposable todos benchmark fixture')
        foreach($n in 1..2){
            $env:GIT_AUTHOR_DATE=$env:GIT_COMMITTER_DATE="2026-10-19T00:00:0${n}Z"
            [IO.File]::WriteAllText((Join-Path $path 'tracked.txt'),"commit $n`n")
            Fixture-Git $path @('add','tracked.txt') | Out-Null
            Fixture-Git $path @('commit','-m',"Fixture commit $n") | Out-Null
        }
        if($Scenario -eq 'todo-graph'){
            Fixture-Git $path @('switch','-c','newer') | Out-Null
            $env:GIT_AUTHOR_DATE=$env:GIT_COMMITTER_DATE='2026-10-19T00:00:03Z'
            [IO.File]::WriteAllText((Join-Path $path 'newer.txt'),"newer graph node`n")
            Fixture-Git $path @('add','newer.txt') | Out-Null
            Fixture-Git $path @('commit','-m','Newer branch above HEAD') | Out-Null
            Fixture-Git $path @('switch','main') | Out-Null
        }
        if($Scenario -like 'stash-*'){
            foreach($n in 1..3){
                [IO.File]::WriteAllText((Join-Path $path 'tracked.txt'),"stashed $n`n")
                [IO.File]::WriteAllText((Join-Path $path "untracked-$n.txt"),"untracked stash $n`n")
                Fixture-Git $path @('stash','push','-u','-m',"Benchmark stash $n") | Out-Null
            }
        }else{
            [IO.File]::WriteAllText((Join-Path $path 'tracked.txt'),"staged`n")
            Fixture-Git $path @('add','tracked.txt') | Out-Null
            [IO.File]::AppendAllText((Join-Path $path 'tracked.txt'),"unstaged`n")
            [IO.File]::WriteAllText((Join-Path $path 'untracked.txt'),"untracked`n")
        }
        return $path
    } finally { foreach($name in $dates.Keys){ if($null-eq$dates[$name]){Remove-Item -LiteralPath ('Env:'+$name) -ErrorAction SilentlyContinue}else{[Environment]::SetEnvironmentVariable($name,$dates[$name],'Process')} } }
}
function Run-One([string]$scenario,[int]$sample,[string]$label,[string]$exe) {
    if ($ToolsDiscoveryFixtureRoot) {
        $root = (Resolve-Path -LiteralPath $ToolsDiscoveryFixtureRoot).Path
        $marker = '.git-rustle-tools-list-fixture'
        $markerText = 'disposable external tools list fixture'
        if (!(Test-Path -LiteralPath (Join-Path $root $marker)) -or [IO.File]::ReadAllText((Join-Path $root $marker)) -cne $markerText) { throw 'Missing disposable tools-list fixture marker' }
        $fixture = Join-Path $root "$label-$scenario-$sample"
        if (Test-Path -LiteralPath $fixture) { throw "Retaining existing first-scan fixture: $fixture" }
        $env:LOCALAPPDATA = Join-Path $fixture 'app-data'
        $code = Join-Path $fixture 'installations/vscode'
        $araxis = Join-Path $fixture 'installations/araxis'
        foreach ($directory in @($env:LOCALAPPDATA,(Join-Path $code 'resources/app/out'),$araxis)) { New-Item -ItemType Directory -Path $directory -Force | Out-Null }
        [IO.File]::WriteAllText((Join-Path $env:LOCALAPPDATA $marker),$markerText)
        [IO.File]::WriteAllBytes((Join-Path $code 'Code.exe'),[byte[]]@())
        [IO.File]::WriteAllBytes((Join-Path $araxis 'Compare.exe'),[byte[]]@())
        [IO.File]::WriteAllText((Join-Path $code 'resources/app/out/cli.js'),'fixture only; never executed')
        $env:GIT_RUSTLE_TOOL_DISCOVERY_ROOTS = @($code,$araxis) -join [IO.Path]::PathSeparator
    }
    # The last viewport's author worker can finish after the 120th frame.
    # Keep the process alive for the final-state assertion; measured intervals
    # still end at desktop scroll done, before this settling period.
    $env:GIT_RUSTLE_AUTOQUIT = if ($scenario -eq 'avatar-reuse-scroll') { '2500' } else { '100' }
    if ($scenario -in @('settings-cache','settings-cache-clear')) {
        if (!$ImageCacheSeed) { throw 'Cache-control scenarios require a disposable ImageCacheSeed and isolated LOCALAPPDATA' }
        $marker = Join-Path $env:LOCALAPPDATA 'cache-benchmark-fixture'
        if (!(Test-Path $marker) -or (Get-Content $marker -Raw) -cne 'disposable image cache benchmark') { throw 'Missing disposable-cache marker' }
        $cache = Join-Path $env:LOCALAPPDATA 'git-rustle/cache/avatars-v1'
        New-Item -ItemType Directory -Force $cache | Out-Null
        Copy-Item -Path (Join-Path $ImageCacheSeed '*.bin') -Destination $cache -Force
    }
    if ($RejectCompetingBuilds) { Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
    $log = Join-Path $OutputDirectory "$label-$scenario-$sample.txt"
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_FRAME_SAMPLES = Join-Path $OutputDirectory "$label-$scenario-$sample-frames.json"
    $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$label-$scenario-$sample-state.txt"
    $fixture = $scenario -in @('todo-graph','stash-panel','stash-apply','stash-delete','stash-create','ref-storage')
    $launchRepository = if($fixture){ New-DisposableFixture $scenario $label $sample }else{$Repository}
    $beforeFixture = if($fixture){ Fixture-State $launchRepository }else{$null}
    if($fixture){[pscustomobject]@{scenario=$scenario;sample=$sample;repository=$launchRepository;before=$beforeFixture}|ConvertTo-Json -Depth 6|Set-Content (Join-Path $OutputDirectory "$label-$scenario-$sample-fixture-before.json")}
    # Display-enabled windows paint on a private desktop that is never activated.
    $p = Start-BenchmarkNativeProcess -FilePath $exe -ArgumentList @('"' + $launchRepository + '"') -RedirectStandardError $log
    try {
        if (-not $p.WaitForExit(60000)) { $p.Kill(); throw "Timeout: $log" }
        $p.Refresh()
        $exitCode = $p.ExitCode
    } finally { $p.Dispose() }
    if ($RejectCompetingBuilds) { Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
    if ($exitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $state = Get-Content -LiteralPath $env:GIT_RUSTLE_DUMP -Raw
    if($RequireSelectedPath -and $label -eq 'candidate' -and $scenario -eq 'graph-hover' -and $state -notmatch 'todos_selected_path_checks=[1-9][0-9]*'){
        throw "Selected commit path did not verify its painted viewport: $log"
    }
    if($label -eq 'candidate' -and $scenario -like 'stash-*'){
        if($state -notmatch 'todos_stash_entry_paints=([0-9]+)' -or [int]$Matches[1] -lt 6){throw "Expected actual stash subjects and actions to paint for at least three entries: $log"}
        $boundsPath=([IO.Path]::ChangeExtension($env:GIT_RUSTLE_FRAME_SAMPLES,'stash-bounds.json'))
        if(!(Test-Path -LiteralPath $boundsPath)){throw "Missing stash entry paint bounds: $boundsPath"}
    }
    if($scenario -in @('todo-graph','branch-sort','graph-hover','stash-panel','stash-apply','stash-delete','stash-create','external-tools','ref-storage') -and $state -notmatch 'todos_verified=true'){
        throw "Todo scenario did not verify its completed frame: $log"
    }
    if($scenario -in @('todo-graph','branch-sort','graph-hover','stash-panel','stash-apply','stash-delete','stash-create','external-tools','ref-storage') -and $state -notmatch 'todos_checks=[1-9][0-9]*'){
        throw "Todo scenario did not record functional checks: $log"
    }
    if($ToolsDiscoveryFixtureRoot -and $scenario-eq'external-tools'){
        if($state-notmatch'todos_tool_list_verified=true'){throw "Extended tool-list actions were not verified: $log"}
        $configPath=Join-Path $env:LOCALAPPDATA 'git-rustle/tools.json'
        $config=Get-Content -LiteralPath $configPath -Raw|ConvertFrom-Json
        if(!$config.scanned-or@($config.tools).Count-ne2-or@($config.tools.id|Sort-Object -Unique).Count-ne2-or@($config.tools|Where-Object name -EQ 'Benchmark saved tool').Count){throw "Expected persisted one-time scan and only original2tools after remove: $log"}
        Copy-Item -LiteralPath $configPath -Destination (Join-Path $OutputDirectory "$label-$scenario-$sample-tools.json")
        [pscustomobject]@{appData=$env:LOCALAPPDATA;roots=$env:GIT_RUSTLE_TOOL_DISCOVERY_ROOTS;scanned=$config.scanned;entries=@($config.tools).Count;ids=@($config.tools.id)}|ConvertTo-Json -Depth 4|Set-Content (Join-Path $OutputDirectory "$label-$scenario-$sample-tools-fixture.json")
    }
    if($fixture){
        $afterFixture=Fixture-State $launchRepository
        [pscustomobject]@{scenario=$scenario;sample=$sample;repository=$launchRepository;before=$beforeFixture;after=$afterFixture}|ConvertTo-Json -Depth 6|Set-Content (Join-Path $OutputDirectory "$label-$scenario-$sample-fixture.json")
        if($beforeFixture.head -cne $afterFixture.head -or $beforeFixture.branch -cne $afterFixture.branch -or $beforeFixture.refs -cne $afterFixture.refs){throw "Fixture HEAD/branches changed: $log"}
        if($scenario -eq 'stash-apply'){
            if($afterFixture.stashes.Count -ne 3 -or (($beforeFixture.stashes -join "`n") -cne ($afterFixture.stashes -join "`n")) -or !$afterFixture.status){throw "Stash apply must retain all3 entries and produce pending changes: $log"}
        }elseif($scenario -eq 'stash-delete'){
            if($afterFixture.stashes.Count -ne 2 -or (($beforeFixture.stashes[1..2] -join "`n") -cne ($afterFixture.stashes -join "`n")) -or $afterFixture.status){throw "Stash delete must remove only the selected newest entry: $log"}
        }elseif($scenario -eq 'stash-create'){
            if($afterFixture.stashes.Count -ne 4 -or $afterFixture.status){throw "Stash creation must leave four entries and a clean worktree: $log"}
        }else{
            if($beforeFixture.index -cne $afterFixture.index -or $beforeFixture.status -cne $afterFixture.status -or (ConvertTo-Json -InputObject @($beforeFixture.files) -Compress) -cne (ConvertTo-Json -InputObject @($afterFixture.files) -Compress)){throw "Fixture index/worktree changed unexpectedly: $log"}
        }
        if($scenario -eq 'stash-panel' -and $afterFixture.stashes.Count -ne 3){throw "Stash panel must retain3 entries: $log"}
        if($scenario -eq 'stash-create' -and ($afterFixture.stashes.Count -ne 4 -or $afterFixture.status)){throw "Stash create/external refresh must leave four stashes and a clean worktree: $log"}
        if($scenario -eq 'ref-storage' -and (Fixture-Git $launchRepository @('rev-parse','--show-ref-format')) -cne 'reftable'){throw "Reference conversion did not finish: $log"}
    }
    if ($scenario -eq 'filter-visuals' -and
        $state -notmatch 'filter_visual_verified=true filter_visual_states=7 branch_owner_verified=true head_badge_painted=true') {
        throw "Filter focus/blur, branch author matches or HEAD paint were not verified: $log"
    }
    if ($scenario -in @('general-settings','general-tabs') -and
        $state -notmatch 'settings_page=3 general_verified=true') {
        throw "General settings did not finish verified on its own page: $log"
    }
    if ($scenario -eq 'general-settings' -and
        ($state -notmatch 'general_update_requests=3 general_update_downloads=0' -or
         $state -notmatch 'image_cache_bytes=0 image_cache_images=0 image_cache_busy=false image_cache_cleared=true')) {
        throw "Manual update checks or isolated cache reset did not finish correctly: $log"
    }
    if ($scenario -eq 'toolbar-counts' -and
        ($state -notmatch 'toolbar_bounds_verified=true' -or $state -notmatch 'toolbar_frames=120' -or
         $state -notmatch 'toolbar_contexts=4' -or $state -notmatch 'toolbar_states=6')) {
        throw "Toolbar count states or bounds were not fully verified: $log"
    }
    if ($scenario -in @('search-focus','branch-search-focus','search-clear','search-clear-multi','provider-scroll','provider-menu','refresh-feedback') -and
        $state -notmatch 'improvement_verified=true') { throw "Native improvement scenario did not verify its final frame: $log" }
    if ($scenario -eq 'search-clear-multi' -and $state -notmatch 'improvement_selected=2(?:\s|$)') {
        throw "Expected two selections preserved after clearing search: $log"
    }
    if ($scenario -in @('provider-scroll','provider-menu') -and $state -notmatch 'provider_badges_rendered=[1-9][0-9]*') {
        throw "Expected rendered provider badges and decoded fixture image: $log"
    }
    if ($scenario -in @('remote-diff','remote-pair') -and
        ($state -notmatch 'diff_files=[1-9][0-9]* diff_failed=false' -or $state -notmatch '[1-9][0-9]* ghost commits' -or
         $text -notmatch 'file diff \((?:[2-9]|[1-9][0-9]+) rows\)')) {
        throw "Expected a loaded remote diff with unfetched commits: $log"
    }
    if ($scenario -eq 'settings-cache' -and $state -notmatch 'image_cache_bytes=[1-9][0-9]* image_cache_images=500 image_cache_busy=false') { throw "Expected 500 seeded images and a completed size scan: $log" }
    if ($scenario -eq 'settings-cache-clear' -and $state -notmatch 'image_cache_bytes=0 image_cache_images=0 image_cache_busy=false image_cache_cleared=true') { throw "Expected an empty cache after clearing: $log" }
    if ($scenario -in @('request-compose','request-edit','request-tabs','request-clear') -and
        ($state -notmatch 'request_open=true request_members=10000 request_sending=false' -or $text -match 'POST ')) {
        throw "Expected a retained composer with 10,000 members and no submission: $log"
    }
    if ($scenario -in @('request-branches','request-branches-menu') -and
        ($state -notmatch 'request_source=FeatureA request_target=Dev request_branches=[1-9][0-9]* request_branches_verified=true' -or $text -match 'POST ')) {
        throw "Expected verified branch selection, closest Dev target and no submission: $log"
    }
    if ($scenario -in @('request-open','request-clear') -and
        ($state -notmatch 'request_open=true .*request_sending=false request_body_bytes=0' -or $text -match 'POST ')) {
        throw "Expected an open composer with an empty description and no submission: $log"
    }
    if ($scenario -eq 'checkout-dialog' -and
        ($state -notmatch 'checkout_choices=[1-9]' -or $text -match 'checkout requested')) {
        throw "Expected an open checkout dialog without a checkout: $log"
    }
    if ($scenario -eq 'status-panels' -and $state -notmatch 'footer_panel=Some\(Details\)') {
        throw "Expected the final toggle to leave Details open: $log"
    }
    if ($scenario -eq 'update-available' -and $state -notmatch 'update_available=true') {
        throw "Expected an available update without installing it: $log"
    }
    if ($scenario -eq 'avatar-reuse-scroll' -and $state -notmatch 'avatar_visible_ready=[1-9]') {
        throw "Expected visible rows to reuse known author profiles: $log"
    }
    if ($scenario -in @('avatar-cache','avatar-cache-busy') -and $state -notmatch 'avatar_visible_ready=[1-9]') {
        throw "Expected a cached photo on the first visible graph row: $log"
    }
    if ($scenario -in @('scroll','search-scroll','graph-resize','tabs','avatar-scroll','avatar-reuse-scroll','provider-scroll','toolbar-counts') -and
        $state -notmatch 'viewport_rows=\[[1-9]') { throw "Graph did not render rows: $log" }
    if ($RequireSyntax -and $label -eq 'candidate' -and $state -notmatch 'syntax_spans=[1-9][0-9]*') {
        throw "Candidate did not install syntax colors: $log"
    }
    if ($ExpectedSyntaxSpans -gt 0 -and $label -eq 'candidate' -and
        ($state -notmatch 'syntax_spans=([0-9]+)' -or [int]$Matches[1] -ne $ExpectedSyntaxSpans)) {
        throw "Candidate did not install the complete expected syntax spans ($ExpectedSyntaxSpans): $log"
    }
    $marks = @{}
    foreach ($m in [regex]::Matches($text,'([\d.]+) ms  ([^\r\n]+)')) {
        if (-not $marks.ContainsKey($m.Groups[2].Value)) { $marks[$m.Groups[2].Value] = [double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    }
    if (-not $marks.ContainsKey('desktop scenario frame')) { throw "Missing rendered scenario frame: $log" }
    $metric = switch ($scenario) {
        { $_ -in 'todo-graph','stash-panel','stash-apply','stash-delete','stash-create','external-tools','ref-storage' } { 'desktop scenario frame' }
        { $_ -in 'branch-sort','graph-hover' } { 'desktop scroll done' }
        { $_ -in 'filter-visuals','general-settings' } { 'desktop scenario frame' }
        'general-tabs' { 'desktop scroll done' }
        { $_ -in 'search-focus','branch-search-focus','search-clear','search-clear-multi','provider-menu','refresh-feedback' } { 'desktop scenario frame' }
        'provider-scroll' { 'desktop scroll done' }
        'toolbar-counts' { 'desktop scroll done' }
        { $_ -in 'settings-cache','settings-cache-clear' } { 'desktop scenario frame' }
        { $_ -in 'avatar-cache','avatar-cache-busy' } { 'desktop scenario frame' }
        'request-compose' { 'request composer ready' }
        'request-open' { 'desktop scenario frame' }
        { $_ -in 'request-branches','request-branches-menu' } { 'request branches verified frame' }
        'request-clear' { 'desktop scroll done' }
        'request-edit' { 'desktop scroll done' }
        'request-tabs' { 'desktop scroll done' }
        'avatar-scroll' { 'desktop scroll done' }
        'avatar-reuse-scroll' { 'desktop scroll done' }
        'scroll' { 'desktop scroll done' }
        'graph-resize' { 'desktop scroll done' }
        'checkout-dialog' { 'desktop scroll done' }
        'files-scroll' { 'desktop scroll done' }
        'diff-scroll' { 'desktop scroll done' }
        'diff-pan' { 'desktop scroll done' }
        'search-scroll' { 'desktop scroll done' }
        'wrap-scroll' { 'desktop scroll done' }
        'tabs' { 'desktop scroll done' }
        'settings-tabs' { 'desktop scroll done' }
        'status-panels' { 'desktop scroll done' }
        'update-available' { 'desktop scroll done' }
        'compare' { 'file diff (' }
        'remote-diff' { 'file diff (' }
        'remote-pair' { 'file diff (' }
        'branches' { 'branch activity ready' }
        'diff' { 'file diff (' }
        'search' { 'search ready' }
        'settings' { 'settings completed' }
        'pending' { 'pending status ready' }
        'pending-diff' { 'file diff (' }
    }
    $end = @($marks.Keys | Where-Object { $_.StartsWith($metric) } | Sort-Object)[0]
    if (-not $end) { throw "Missing $metric in $log" }
    $start = switch ($scenario) {
        { $_ -in 'todo-graph','stash-panel','stash-apply','stash-delete','stash-create','external-tools','ref-storage' } { 'desktop scenario requested' }
        'filter-visuals' { 'desktop filter requested' }
        'general-settings' { 'general settings requested' }
        { $_ -in 'search-focus','branch-search-focus','search-clear','search-clear-multi','provider-menu','refresh-feedback' } { 'desktop improvement requested' }
        'settings-cache' { 'image cache requested' }
        'settings-cache-clear' { 'image cache clear requested' }
        { $_ -in 'avatar-cache','avatar-cache-busy' } { 'avatar cache requested' }
        'request-compose' { 'request composer requested' }
        'request-open' { 'request composer requested' }
        { $_ -in 'request-branches','request-branches-menu' } { 'request source change requested' }
        'branches' { 'branch activity requested' }
        'diff' { 'file diff requested' }
        'search' {
            if ($end -notmatch '^search ready \((\d+);') { throw "Unrecognized search result: $end" }
            "search requested ($($Matches[1]))"
        }
        'settings' { 'settings requested' }
        'pending' { 'pending status requested' }
        'pending-diff' { 'file diff requested' }
        'compare' { 'file diff requested' }
        'remote-diff' { 'remote diff requested' }
        'remote-pair' { 'remote diff requested' }
    }
    $scrolling = $scenario -in @('scroll','graph-resize','files-scroll','diff-scroll','diff-pan','search-scroll','wrap-scroll','tabs','settings-tabs','general-tabs','checkout-dialog','status-panels','request-edit','request-tabs','request-clear','avatar-scroll','avatar-reuse-scroll','update-available','provider-scroll','toolbar-counts','branch-sort','graph-hover')
    $frames = if ($scrolling) { @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json) } else { @() }
    if ($scrolling -and $frames.Count -ne 120) { throw 'Expected 120 measured frames' }
    $todoHandler = if($state -match 'todo_handler_ms=(\[[^\]\r\n]*\])'){@($Matches[1]|ConvertFrom-Json)}else{@()}
    if($scenario -in @('branch-sort','graph-hover')){
        if($state -notmatch 'todos_frames=120' -or $todoHandler.Count -ne 120){throw "Expected120 actual todo handlers and rendered frames: $log"}
        $boundsPath=[IO.Path]::ChangeExtension($env:GIT_RUSTLE_FRAME_SAMPLES,'todos-bounds.json')
        $observations=@(Get-Content -LiteralPath $boundsPath -Raw|ConvertFrom-Json)
        if($observations.Count -ne 120 -or @($observations.frame|Sort-Object -Unique).Count -ne 120 -or @($observations|Where-Object{$_.bounds.Count -ne 4 -or $_.bounds[2] -le 0 -or $_.bounds[3] -le 0}).Count){throw "Expected120 nonempty todo paint observations: $log"}
    }
    $toolbarHandler = if ($state -match 'toolbar_handler_ms=(\[[^\]\r\n]*\])') { @($Matches[1] | ConvertFrom-Json) } else { @() }
    $generalMetrics = if ($scenario -eq 'general-settings') {
        $checks = if ($state -match 'general_checks_ms=(\[[^\]\r\n]*\])') { @($Matches[1] | ConvertFrom-Json) } else { @() }
        if ($checks.Count -ne 3) { throw 'Expected current/available/error manual-check frame timings' }
        if ($state -notmatch 'general_cache_clear_frame_ms=Some\(([\d.]+)\)') { throw 'Missing cache-clear-to-frame duration' }
        [pscustomobject]@{currentCheckMs=$checks[0];availableCheckMs=$checks[1];errorCheckMs=$checks[2];cacheClearFrameMs=[double]::Parse($Matches[1],[cultureinfo]::InvariantCulture)}
    } else { $null }
    $toolListMetrics = if ($ToolsDiscoveryFixtureRoot -and $scenario -eq 'external-tools') {
        $intervals = [ordered]@{
            firstScanMs=@('external tools initialization requested','external tools initialization ready')
            configureMs=@('tools list configure requested','tools list configure painted')
            saveMs=@('tools list save requested','tools list saved frame')
            selectionMs=@('tools list selection requested','tools list selected frame')
            removeMs=@('tools list remove requested','tools list removed frame')
            pickerMs=@('tools diff picker requested','tools diff picker painted')
        }
        $values=[ordered]@{}
        foreach($field in $intervals.Keys){
            $pair=$intervals[$field]
            if(!$marks.ContainsKey($pair[0])-or!$marks.ContainsKey($pair[1])){throw "Missing first-scan/list interval $field in $log"}
            $values[$field]=$marks[$pair[1]]-$marks[$pair[0]]
            if($values[$field]-lt0){throw "Reversed first-scan/list interval $field"}
        }
        [pscustomobject]$values
    } else { $null }
    if ($scenario -eq 'toolbar-counts' -and $toolbarHandler.Count -ne 24) { throw 'Expected 24 toolbar state-transition CPU samples' }
    $syntax = @($marks.Keys | Where-Object { $_.StartsWith('diff syntax ready (') } | Sort-Object)[0]
    $syntaxMs = if ($syntax -and $marks.ContainsKey('file diff requested')) { $marks[$syntax] - $marks['file diff requested'] } else { $null }
    $presentedMs = if ($marks.ContainsKey('file diff requested')) { $marks['desktop scenario frame'] - $marks['file diff requested'] } else { $null }
    [pscustomobject]@{toolListMetrics=$toolListMetrics;todoHandlerMs=$todoHandler;generalMetrics=$generalMetrics;toolbarHandlerMs=$toolbarHandler;syntaxReadyMs=$syntaxMs;scenarioFrameAfterRequestMs=$presentedMs;binary=$label;scenario=$scenario;sample=$sample;readySinceMainMs=$marks[$end];workerMs=if($start){$marks[$end]-$marks[$start]}else{$null};frameIntervalsMs=$frames;wrapLayoutMs=if($text -match "wrap layout ([\d.]+) ms"){[double]::Parse($Matches[1],[cultureinfo]::InvariantCulture)}else{$null}}
}
try {
    $env:GIT_RUSTLE_TIMING='1'; $env:GIT_RUSTLE_AUTOQUIT='100'
    Remove-Item Env:GIT_RUSTLE_TRACE -ErrorAction SilentlyContinue
    $results=@()
    foreach ($scenario in $Scenarios) {
        if ($Baseline) { Run-One $scenario -1 'baseline' $Baseline | Out-Null }
        Run-One $scenario -1 'candidate' $Candidate | Out-Null
        for ($i=0;$i -lt $Samples;$i++) {
            if ($Baseline -and $i % 2 -eq 0) { $results += Run-One $scenario $i 'baseline' $Baseline }
            $results += Run-One $scenario $i 'candidate' $Candidate
            if ($Baseline -and $i % 2 -ne 0) { $results += Run-One $scenario $i 'baseline' $Baseline }
        }
    }
    $results | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutputDirectory 'samples.json')
} finally { foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') } }
