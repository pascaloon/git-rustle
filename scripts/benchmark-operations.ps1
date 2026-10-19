param(
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [switch]$RejectCompetingBuilds,
    [string[]]$Scenarios = @('operations','operations-create','rebase-plan','history'),
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Use a fresh output directory.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
function Git([string]$path, [string[]]$arguments) {
    $text = & git.exe -C $path @arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw ($text -join "`n") }
    return ($text -join "`n")
}
function Run-One([string]$scenario, [int]$sample) {
    $root = Join-Path $OutputDirectory "$scenario-$sample-repository"
    New-Item -ItemType Directory $root | Out-Null
    Git $root @('init','-q','-b','main') | Out-Null
    Git $root @('config','user.name','Rustle Benchmark') | Out-Null
    Git $root @('config','user.email','bench@example.invalid') | Out-Null
    Git $root @('config','commit.gpgSign','false') | Out-Null
    Git $root @('config','core.hooksPath','disabled-hooks') | Out-Null
    foreach ($i in 0..3) {
        [IO.File]::WriteAllText((Join-Path $root "file-$i.txt"), "content $i`n")
        Git $root @('add','.') | Out-Null
        Git $root @('commit','-qm',"Commit $i") | Out-Null
    }
    if ($scenario -in @('tag-dialog','tag-create')) {
        $headBefore = Git $root @('rev-parse','HEAD')
        $previousCommit = Git $root @('rev-parse','HEAD~1')
        $headsBefore = Git $root @('for-each-ref','--format=%(refname) %(objectname)','refs/heads')
    }
    if ($scenario -like 'push-review*') {
        $remote = Join-Path $OutputDirectory "$scenario-$sample-remote.git"
        Git $root @('clone','--bare',$root,$remote) | Out-Null
        Git $root @('remote','add','origin',$remote) | Out-Null
        Git $root @('fetch','origin') | Out-Null
        Git $root @('branch','--set-upstream-to=origin/main') | Out-Null
        if ($scenario -eq 'push-review-orange') { Git $root @('commit','--amend','-qm','Rewritten tip') | Out-Null }
        if ($scenario -eq 'push-review-red') {
            Git $root @('update-ref','refs/remotes/origin/main','HEAD~1') | Out-Null
            Git $root @('reset','--hard','HEAD~1') | Out-Null
        }
    }
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$scenario-$sample-state.txt"
    $log = Join-Path $OutputDirectory "$scenario-$sample.txt"
    if ($RejectCompetingBuilds -and @(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: competing build detected' }
    # Visible native frames are required by the rendering benchmark workflow.
    $process = Start-Process -FilePath $Candidate -ArgumentList @('"' + $root + '"') -WindowStyle Normal -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(60000)) { Stop-Process -Id $process.Id; throw "Timed out: $log" }
    $process.Refresh()
    if ($RejectCompetingBuilds -and @(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: competing build detected' }
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $start = [regex]::Match($text, '([\d.]+) ms  desktop operation requested')
    $end = [regex]::Match($text, '([\d.]+) ms  desktop scenario frame')
    if (-not $start.Success -or -not $end.Success) { throw "Missing timing marks: $log" }
    if ($scenario -eq 'operations-create') {
        if ((Git $root @('branch','--list','bench-topic')) -notmatch 'bench-topic') { throw 'Branch was not created.' }
        if ((Get-Content $env:GIT_RUSTLE_DUMP -Raw) -notmatch 'operations=1 cursor=1') { throw 'History entry missing.' }
    }
    $state = Get-Content $env:GIT_RUSTLE_DUMP -Raw
    if ($scenario -in @('tag-dialog','tag-create')) {
        if ($state -notmatch 'tag_scenario_verified=true') { throw "Tag scenario failed native validation: $log" }
        if ((Git $root @('rev-parse','HEAD')) -ne $headBefore -or
            (Git $root @('for-each-ref','--format=%(refname) %(objectname)','refs/heads')) -ne $headsBefore -or
            (Git $root @('status','--porcelain=v1'))) { throw 'Tag scenario changed HEAD, branches or worktree' }
        if ($scenario -eq 'tag-dialog') {
            if ($state -notmatch 'dialog=Some\(CreateTag\)' -or (Git $root @('tag','--list','bench-tag'))) {
                throw 'Expected an unsubmitted tag dialog and no created tag'
            }
        } else {
            if ($state -notmatch 'tag_scenario_created=true tag_scenario_label=true' -or
                (Git $root @('rev-parse','bench-tag^{commit}')) -ne $previousCommit -or
                (Git $root @('cat-file','-t','refs/tags/bench-tag')) -ne 'tag' -or
                (Git $root @('for-each-ref','--format=%(contents:subject)','refs/tags/bench-tag')) -ne 'Benchmark annotation') {
                throw 'Expected an annotated tag at the frozen selected commit and its refreshed graph label'
            }
        }
    }
    $expected = switch ($scenario) { 'push-review' { 'Trivial' }; 'push-review-orange' { 'Rewrite' }; 'push-review-red' { 'Blocked' } }
    if ($expected -and $state -notmatch "push=Some\($expected\)") { throw "Unexpected push review: $state" }
    if ($scenario -eq 'rebase-plan' -and $state -notmatch 'rebase_open=true') { throw 'Rebase plan tab missing.' }
    [pscustomobject]@{scenario=$scenario;sample=$sample;actionToFrameMs=([double]::Parse($end.Groups[1].Value,[cultureinfo]::InvariantCulture)-[double]::Parse($start.Groups[1].Value,[cultureinfo]::InvariantCulture))}
}
try {
    $env:GIT_RUSTLE_TIMING = '1'; $env:GIT_RUSTLE_AUTOQUIT = '100'
    $results = @()
    foreach ($scenario in $Scenarios) {
        Run-One $scenario -1 | Out-Null
        for ($sample=0; $sample -lt $Samples; $sample++) { $results += Run-One $scenario $sample }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
