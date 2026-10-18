param(
    [Parameter(Mandatory)][string]$Repository,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [string]$Baseline,
    [switch]$RequireSyntax,
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
    if ($scenario -in @('request-compose','request-edit','request-tabs') -and
        ($state -notmatch 'request_open=true request_members=10000 request_sending=false' -or $text -match 'POST ')) {
        throw "Expected a retained composer with 10,000 members and no submission: $log"
    }
    if ($scenario -eq 'checkout-dialog' -and
        ($state -notmatch 'checkout_choices=[1-9]' -or $text -match 'checkout requested')) {
        throw "Expected an open checkout dialog without a checkout: $log"
    }
    if ($scenario -eq 'status-panels' -and $state -notmatch 'footer_panel=Some\(Details\)') {
        throw "Expected the final toggle to leave Details open: $log"
    }
    if ($scenario -eq 'avatar-reuse-scroll' -and $state -notmatch 'avatar_visible_ready=[1-9]') {
        throw "Expected visible rows to reuse known author profiles: $log"
    }
    if ($scenario -in @('scroll','search-scroll','graph-resize','tabs','avatar-scroll','avatar-reuse-scroll') -and
        $state -notmatch 'viewport_rows=\[[1-9]') { throw "Graph did not render rows: $log" }
    if ($RequireSyntax -and $label -eq 'candidate' -and $state -notmatch 'syntax_spans=[1-9][0-9]*') {
        throw "Candidate did not install syntax colors: $log"
    }
    $marks = @{}
    foreach ($m in [regex]::Matches($text,'([\d.]+) ms  ([^\r\n]+)')) {
        if (-not $marks.ContainsKey($m.Groups[2].Value)) { $marks[$m.Groups[2].Value] = [double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    }
    if (-not $marks.ContainsKey('desktop scenario frame')) { throw "Missing rendered scenario frame: $log" }
    $metric = switch ($scenario) {
        'request-compose' { 'request composer ready' }
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
        'compare' { 'file diff (' }
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
        'request-compose' { 'request composer requested' }
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
    }
    $scrolling = $scenario -in @('scroll','graph-resize','files-scroll','diff-scroll','diff-pan','search-scroll','wrap-scroll','tabs','settings-tabs','checkout-dialog','status-panels','request-edit','request-tabs','avatar-scroll','avatar-reuse-scroll')
    $frames = if ($scrolling) { @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json) } else { @() }
    if ($scrolling -and $frames.Count -ne 120) { throw 'Expected 120 measured frames' }
    $syntax = @($marks.Keys | Where-Object { $_.StartsWith('diff syntax ready (') } | Sort-Object)[0]
    $syntaxMs = if ($syntax -and $marks.ContainsKey('file diff requested')) { $marks[$syntax] - $marks['file diff requested'] } else { $null }
    $presentedMs = if ($marks.ContainsKey('file diff requested')) { $marks['desktop scenario frame'] - $marks['file diff requested'] } else { $null }
    [pscustomobject]@{syntaxReadyMs=$syntaxMs;scenarioFrameAfterRequestMs=$presentedMs;binary=$label;scenario=$scenario;sample=$sample;readySinceMainMs=$marks[$end];workerMs=if($start){$marks[$end]-$marks[$start]}else{$null};frameIntervalsMs=$frames;wrapLayoutMs=if($text -match "wrap layout ([\d.]+) ms"){[double]::Parse($Matches[1],[cultureinfo]::InvariantCulture)}else{$null}}
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
