param(
    [Parameter(Mandatory)][string]$Repository,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [string]$Baseline,
    [string]$ImageCacheSeed,
    [switch]$RequireSyntax,
    [ValidateRange(0,200000)][int]$ExpectedSyntaxSpans = 0,
    [switch]$RejectCompetingBuilds,
    [int]$Samples = 15,
    [string[]]$Scenarios = @('scroll','branches','diff','search','settings'),
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\desktop"
)
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
if ($Baseline) { $Baseline = (Resolve-Path -LiteralPath $Baseline).Path }
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
$names = @('GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES')
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
function Run-One([string]$scenario,[int]$sample,[string]$label,[string]$exe) {
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
    if ($RejectCompetingBuilds -and @(Get-Process rustc,cargo,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: competing build detected' }
    $log = Join-Path $OutputDirectory "$label-$scenario-$sample.txt"
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_FRAME_SAMPLES = Join-Path $OutputDirectory "$label-$scenario-$sample-frames.json"
    $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$label-$scenario-$sample-state.txt"
    # These are the actual visible UI frames under test; a hidden GPUI window
    # does not reliably receive presentation callbacks.
    $p = Start-Process -FilePath $exe -ArgumentList @('"' + $Repository + '"') -WindowStyle Normal -RedirectStandardError $log -PassThru
    if (-not $p.WaitForExit(60000)) { Stop-Process -Id $p.Id; throw "Timeout: $log" }
    $p.Refresh()
    if ($RejectCompetingBuilds -and @(Get-Process rustc,cargo,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: competing build detected' }
    if ($p.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $state = Get-Content -LiteralPath $env:GIT_RUSTLE_DUMP -Raw
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
    if ($scenario -in @('search-focus','branch-search-focus','search-clear','search-clear-multi','provider-scroll','provider-menu') -and
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
        { $_ -in 'filter-visuals','general-settings' } { 'desktop scenario frame' }
        'general-tabs' { 'desktop scroll done' }
        { $_ -in 'search-focus','branch-search-focus','search-clear','search-clear-multi','provider-menu' } { 'desktop scenario frame' }
        'provider-scroll' { 'desktop scroll done' }
        'toolbar-counts' { 'desktop scroll done' }
        { $_ -in 'settings-cache','settings-cache-clear' } { 'desktop scenario frame' }
        { $_ -in 'avatar-cache','avatar-cache-busy' } { 'desktop scenario frame' }
        'request-compose' { 'request composer ready' }
        'request-open' { 'desktop scenario frame' }
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
        'filter-visuals' { 'desktop filter requested' }
        'general-settings' { 'general settings requested' }
        { $_ -in 'search-focus','branch-search-focus','search-clear','search-clear-multi','provider-menu' } { 'desktop improvement requested' }
        'settings-cache' { 'image cache requested' }
        'settings-cache-clear' { 'image cache clear requested' }
        { $_ -in 'avatar-cache','avatar-cache-busy' } { 'avatar cache requested' }
        'request-compose' { 'request composer requested' }
        'request-open' { 'request composer requested' }
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
    $scrolling = $scenario -in @('scroll','graph-resize','files-scroll','diff-scroll','diff-pan','search-scroll','wrap-scroll','tabs','settings-tabs','general-tabs','checkout-dialog','status-panels','request-edit','request-tabs','request-clear','avatar-scroll','avatar-reuse-scroll','update-available','provider-scroll','toolbar-counts')
    $frames = if ($scrolling) { @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json) } else { @() }
    if ($scrolling -and $frames.Count -ne 120) { throw 'Expected 120 measured frames' }
    $toolbarHandler = if ($state -match 'toolbar_handler_ms=(\[[^\]\r\n]*\])') { @($Matches[1] | ConvertFrom-Json) } else { @() }
    $generalMetrics = if ($scenario -eq 'general-settings') {
        $checks = if ($state -match 'general_checks_ms=(\[[^\]\r\n]*\])') { @($Matches[1] | ConvertFrom-Json) } else { @() }
        if ($checks.Count -ne 3) { throw 'Expected current/available/error manual-check frame timings' }
        if ($state -notmatch 'general_cache_clear_frame_ms=Some\(([\d.]+)\)') { throw 'Missing cache-clear-to-frame duration' }
        [pscustomobject]@{currentCheckMs=$checks[0];availableCheckMs=$checks[1];errorCheckMs=$checks[2];cacheClearFrameMs=[double]::Parse($Matches[1],[cultureinfo]::InvariantCulture)}
    } else { $null }
    if ($scenario -eq 'toolbar-counts' -and $toolbarHandler.Count -ne 24) { throw 'Expected 24 toolbar state-transition CPU samples' }
    $syntax = @($marks.Keys | Where-Object { $_.StartsWith('diff syntax ready (') } | Sort-Object)[0]
    $syntaxMs = if ($syntax -and $marks.ContainsKey('file diff requested')) { $marks[$syntax] - $marks['file diff requested'] } else { $null }
    $presentedMs = if ($marks.ContainsKey('file diff requested')) { $marks['desktop scenario frame'] - $marks['file diff requested'] } else { $null }
    [pscustomobject]@{generalMetrics=$generalMetrics;toolbarHandlerMs=$toolbarHandler;syntaxReadyMs=$syntaxMs;scenarioFrameAfterRequestMs=$presentedMs;binary=$label;scenario=$scenario;sample=$sample;readySinceMainMs=$marks[$end];workerMs=if($start){$marks[$end]-$marks[$start]}else{$null};frameIntervalsMs=$frames;wrapLayoutMs=if($text -match "wrap layout ([\d.]+) ms"){[double]::Parse($Matches[1],[cultureinfo]::InvariantCulture)}else{$null}}
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
