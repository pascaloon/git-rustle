param(
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15
)
$ErrorActionPreference = 'Stop'
$Fixture = (Resolve-Path -LiteralPath $Fixture).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$marker = Join-Path $Fixture '.git/rustle-undo-benchmark'
if (-not (Test-Path -LiteralPath $marker) -or (Get-Content $marker -Raw).Trim() -ne 'undo fixture 1') { throw 'Use prepare_undo_fixture, never a user repository.' }
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Use a fresh output directory.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$journal = Join-Path $Fixture '.git/rustle-operations/journal.json'
$original = Join-Path $Fixture '.git/rustle-operations/benchmark-journal.json'
$seed = Get-Content $original -Raw | ConvertFrom-Json
if ($seed.entries.Count -ne 100 -or $seed.cursor -ne 80) { throw 'Expected a 100-step history with 20 redo steps.' }
function Git([string[]]$arguments) {
    $result = & git.exe -C $Fixture @arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw ($result -join "`n") }
    $result -join "`n"
}
$head = Git @('rev-parse','HEAD')
$refs = Git @('show-ref')
$staged = Git @('ls-files','--stage')
$configHash = (Get-FileHash (Join-Path $Fixture '.git/config')).Hash
$fileHash = (Get-FileHash (Join-Path $Fixture 'file.txt')).Hash
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES','GIT_RUSTLE_RECENTS_FILE')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Run-One([string]$scenario,[int]$sample,[string]$label,[string]$exe) {
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Competing build detected' }
    Copy-Item -LiteralPath $original -Destination $journal
    $prefix = Join-Path $OutputDirectory "$label-$scenario-$sample"
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_DUMP = "$prefix-state.txt"
    $env:GIT_RUSTLE_FRAME_SAMPLES = "$prefix-frames.json"
    # Visible windows are needed for meaningful native frame callbacks.
    $process = Start-Process -FilePath $exe -ArgumentList @('"' + $Fixture + '"') -WindowStyle Normal -RedirectStandardError "$prefix.txt" -PassThru
    if (-not $process.WaitForExit(60000)) { Stop-Process -Id $process.Id; throw "Timed out: $prefix" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $prefix" }
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Competing build detected' }
    $log = Get-Content "$prefix.txt" -Raw
    $state = Get-Content $env:GIT_RUSTLE_DUMP -Raw
    $expected = if ($scenario -eq 'history-clear') { 'operations=0 cursor=0' } else { 'operations=100 cursor=80' }
    if ($state -notmatch $expected -or $state -notmatch 'footer_panel=Some\(History\)') { throw "Wrong history state: $prefix" }
    if ($label -eq 'candidate' -and $scenario -ne 'history-clear' -and $state -notmatch 'history_visible=[1-9][0-9]*') { throw "Empty history viewport: $prefix" }
    $startName = if ($scenario -eq 'history-clear') { 'desktop clear requested' } else { 'desktop operation requested' }
    $start = [regex]::Match($log,"([\d.]+) ms  $startName")
    $end = [regex]::Match($log,'([\d.]+) ms  desktop scenario frame')
    if (-not $start.Success -or -not $end.Success) { throw "Missing marks: $prefix" }
    $frames = @()
    if ($scenario -eq 'history-scroll') {
        $frames = @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json)
        if ($frames.Count -ne 120) { throw "Missing frames: $prefix" }
    }
    $stored = Get-Content $journal -Raw | ConvertFrom-Json
    if ($scenario -eq 'history-clear' -and ($stored.entries.Count -ne 0 -or $stored.cursor -ne 0 -or $stored.pending)) { throw "Clear was not durable: $prefix" }
    if ((Git @('rev-parse','HEAD')) -ne $head -or (Git @('show-ref')) -ne $refs -or
        (Git @('ls-files','--stage')) -ne $staged -or (Git @('status','--porcelain')) -or
        (Get-FileHash (Join-Path $Fixture '.git/config')).Hash -ne $configHash -or
        (Get-FileHash (Join-Path $Fixture 'file.txt')).Hash -ne $fileHash) { throw "Repository changed: $prefix" }
    $worker = [regex]::Matches($log,'([\d.]+) ms  git operation done')
    [pscustomobject]@{
        binary=$label;scenario=$scenario;sample=$sample
        actionToFrameMs=([double]::Parse($end.Groups[1].Value,[cultureinfo]::InvariantCulture)-[double]::Parse($start.Groups[1].Value,[cultureinfo]::InvariantCulture))
        clearWorkerMs=if($scenario -eq 'history-clear'){[double]::Parse($worker[$worker.Count-1].Groups[1].Value,[cultureinfo]::InvariantCulture)-[double]::Parse($start.Groups[1].Value,[cultureinfo]::InvariantCulture)}else{$null}
        frameIntervalsMs=$frames
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'; $env:GIT_RUSTLE_AUTOQUIT='100'
    $env:GIT_RUSTLE_RECENTS_FILE=Join-Path $OutputDirectory 'recents.json'
    $results=@()
    Run-One 'history' -1 'baseline' $Baseline | Out-Null
    Run-One 'history' -1 'candidate' $Candidate | Out-Null
    for ($i=0;$i -lt $Samples;$i++) {
        if ($i % 2 -eq 0) { $results += Run-One 'history' $i 'baseline' $Baseline }
        $results += Run-One 'history' $i 'candidate' $Candidate
        if ($i % 2 -ne 0) { $results += Run-One 'history' $i 'baseline' $Baseline }
        $results | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory 'samples.json')
    }
    foreach ($scenario in @('history-scroll','history-clear')) {
        Run-One $scenario -1 'candidate' $Candidate | Out-Null
        for ($i=0;$i -lt $Samples;$i++) {
            $results += Run-One $scenario $i 'candidate' $Candidate
            $results | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory 'samples.json')
        }
    }
    Get-FileHash $Baseline,$Candidate | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'executables.json')
} finally {
    Copy-Item -LiteralPath $original -Destination $journal
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
