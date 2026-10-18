param(
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [int]$Samples = 15
)
$ErrorActionPreference = 'Stop'
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Use a fresh output directory.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$root = Join-Path $OutputDirectory 'repository'
New-Item -ItemType Directory -Path $root | Out-Null
function Git([string[]]$arguments) {
    $result = & git.exe -C $root @arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw ($result -join "`n") }
    $result -join "`n"
}
Git @('init','-q','-b','main') | Out-Null
Git @('config','user.name','Rustle Benchmark') | Out-Null
Git @('config','user.email','bench@example.invalid') | Out-Null
Git @('config','commit.gpgSign','false') | Out-Null
Git @('config','core.hooksPath','disabled-hooks') | Out-Null
# One Git process builds a linear history; no user repository is modified.
$import = [Text.StringBuilder]::new()
foreach ($i in 0..1000) {
    $message = "Commit $i - refine the interactive rebase workflow"
    [void]$import.Append("commit refs/heads/main`ncommitter Rustle Benchmark <bench@example.invalid> $((1700000000 + $i)) +0000`ndata $($message.Length)`n$message`n`n")
}
[void]$import.Append("done`n")
$import.ToString() | & git.exe -C $root fast-import --quiet
if ($LASTEXITCODE -ne 0) { throw 'Fixture import failed' }
Git @('reset','--hard','HEAD') | Out-Null
$head = Git @('rev-parse','HEAD')
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES','GIT_RUSTLE_REBASE_BASE','GIT_RUSTLE_RECENTS_FILE')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Run-One([string]$scenario, [int]$sample, [string]$label, [string]$exe) {
    if (@(Get-Process rustc,cargo,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Competing build detected' }
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_REBASE_BASE = if ($scenario -eq 'rebase-plan') { 'HEAD~3' } else { 'HEAD~1000' }
    $prefix = Join-Path $OutputDirectory "$label-$scenario-$sample"
    $env:GIT_RUSTLE_FRAME_SAMPLES = "$prefix-frames.json"
    $env:GIT_RUSTLE_DUMP = "$prefix-state.txt"
    # GPUI needs visible windows to measure next-frame callbacks.
    $p = Start-Process -FilePath $exe -ArgumentList @('"' + $root + '"') -WindowStyle Normal -RedirectStandardError "$prefix.txt" -PassThru
    if (-not $p.WaitForExit(60000)) { Stop-Process -Id $p.Id; throw "Timeout: $prefix" }
    $p.Refresh()
    if ($p.ExitCode -ne 0) { throw "Failed: $prefix" }
    if (@(Get-Process rustc,cargo,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Competing build detected' }
    $log = Get-Content "$prefix.txt" -Raw
    $state = Get-Content $env:GIT_RUSTLE_DUMP -Raw
    if ($state -notmatch 'rebase_open=true' -or $state -notmatch 'operations=0 cursor=0' -or $log -match 'interactive rebase requested') { throw "Unexpected rebase state: $prefix" }
    if ($label -eq 'candidate' -and $state -notmatch 'plan_visible=[1-9][0-9]*') { throw "Rebase list did not render rows: $prefix" }
    $start = [regex]::Match($log,'([\d.]+) ms  desktop operation requested')
    $end = [regex]::Match($log,'([\d.]+) ms  desktop scenario frame')
    if (-not $start.Success -or -not $end.Success) { throw "Missing timing marks: $prefix" }
    $frames = @()
    if ($scenario -ne 'rebase-plan') {
        $frames = @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json)
        if ($frames.Count -ne 120 -or $state -notmatch 'plan_rows=1000 plan_visible=[1-9][0-9]* plan_errors=0') { throw "Invalid interaction sample: $prefix" }
    }
    if ((Git @('rev-parse','HEAD')) -ne $head -or (Git @('status','--porcelain'))) { throw 'Benchmark changed the fixture' }
    [pscustomobject]@{
        binary=$label; scenario=$scenario; sample=$sample
        actionToFrameMs=([double]::Parse($end.Groups[1].Value,[cultureinfo]::InvariantCulture)-[double]::Parse($start.Groups[1].Value,[cultureinfo]::InvariantCulture))
        frameIntervalsMs=$frames
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'; $env:GIT_RUSTLE_AUTOQUIT='100'
    $env:GIT_RUSTLE_RECENTS_FILE = Join-Path $OutputDirectory 'recents.json'
    $results = @()
    Run-One 'rebase-plan' -1 'baseline' $Baseline | Out-Null
    Run-One 'rebase-plan' -1 'candidate' $Candidate | Out-Null
    for ($i=0; $i -lt $Samples; $i++) {
        if ($i % 2 -eq 0) { $results += Run-One 'rebase-plan' $i 'baseline' $Baseline }
        $results += Run-One 'rebase-plan' $i 'candidate' $Candidate
        if ($i % 2 -ne 0) { $results += Run-One 'rebase-plan' $i 'baseline' $Baseline }
        $results | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory 'samples.json')
    }
    foreach ($scenario in @('rebase-scroll','rebase-edit')) {
        Run-One $scenario -1 'candidate' $Candidate | Out-Null
        for ($i=0; $i -lt $Samples; $i++) {
            $results += Run-One $scenario $i 'candidate' $Candidate
            $results | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory 'samples.json')
        }
    }
    Get-FileHash $Baseline,$Candidate | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'executables.json')
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
