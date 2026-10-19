param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Toolbar','Tags','Workers')]
    [string[]]$Stages = @('Startup','History','Toolbar','Tags','Workers'),
    [string[]]$SharedComponentTests = @('engine::tests::benchmark_desktop_mutations','git::operations::tests::benchmark_large_worktree_operations')
)
# Reuse a prepared read-only large-history fixture, but always collect fresh
# measurements. Builders and unrelated operation suites never run in this script.
$ErrorActionPreference = 'Stop'
foreach ($name in @('Repository','Baseline','BaselineHarness','CandidateHarness','Candidate')) {
    Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('LOCALAPPDATA','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_BENCH_REPO')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Assert-Idle {
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Timing rejected: finish builds before measuring' }
}
function New-Stage([string]$Name) {
    $path = Join-Path $OutputDirectory $Name
    if (Test-Path -LiteralPath $path) { throw "Retaining existing $Name results; choose a new output directory or omit this stage" }
    New-Item -ItemType Directory $path | Out-Null
    return $path
}
function Invoke-Harness([string]$Exe,[string]$Test,[string]$Log) {
    Assert-Idle
    & $Exe $Test --ignored --exact --nocapture 2>&1 | Tee-Object -FilePath $Log
    if ($LASTEXITCODE -ne 0) { throw "Failed: $Test (retained $Log)" }
    Assert-Idle
    $text = Get-Content -LiteralPath $Log -Raw
    if ($text -notmatch 'running 1 test' -or $text -notmatch 'test result: ok\. 1 passed;') {
        throw "Expected exactly one successful test: $Log"
    }
}
try {
    Assert-Idle
    $env:LOCALAPPDATA = Join-Path $OutputDirectory 'app-data'
    New-Item -ItemType Directory -Force $env:LOCALAPPDATA | Out-Null
    $env:GIT_RUSTLE_BENCH_REPO = $Repository
    Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    $executables = foreach ($name in @('Baseline','BaselineHarness','Candidate','CandidateHarness')) {
        $path = Get-Variable $name -ValueOnly
        [pscustomobject]@{role=$name;path=$path;bytes=(Get-Item -LiteralPath $path).Length;sha256=(Get-FileHash -LiteralPath $path).Hash}
    }
    $sourceHead = & git -C "$PSScriptRoot/.." rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot record source revision' }
    $sourceStatus = @(& git -C "$PSScriptRoot/.." status --porcelain=v1)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot record source status' }
    [pscustomobject]@{
        recordedUtc=[DateTime]::UtcNow.ToString('o'); repository=$Repository
        executables=$executables;sourceHead=$sourceHead;sourceStatus=$sourceStatus
        samples=$Samples;stages=$Stages;sharedComponentTests=$SharedComponentTests
        machine=[Environment]::MachineName;os=[Environment]::OSVersion.ToString();logicalProcessors=[Environment]::ProcessorCount
        componentOrder='One complete internally warmed harness per executable; baseline/candidate order reverses between shared harnesses. Component samples within a harness are not interleaved.'
        cache=$env:LOCALAPPDATA
    } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory "provenance-$([Guid]::NewGuid().ToString('N')).json")
    if ('Startup' -in $Stages) {
        & "$PSScriptRoot/benchmark.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate `
            -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'startup')
    }
    if ('History' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate `
            -Scenarios @('scroll','search-scroll','search') -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'history')
    }
    if ('Toolbar' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate `
            -Scenarios @('toolbar-counts') -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'actions')
    }
    if ('Tags' -in $Stages) {
        $tags = Join-Path $OutputDirectory 'tags-native'
        if (Test-Path -LiteralPath $tags) { throw 'Retaining existing native tag results; choose a new output directory or omit Tags' }
        & "$PSScriptRoot/benchmark-operations.ps1" -Candidate $Candidate -Scenarios @('tag-dialog','tag-create') `
            -RejectCompetingBuilds -Samples $Samples -OutputDirectory $tags
    }
    if ('Workers' -in $Stages) {
        $components = New-Stage 'components'
        for ($i=0; $i -lt $SharedComponentTests.Count; $i++) {
            $test = $SharedComponentTests[$i]
            $labels = if ($i % 2 -eq 0) { @('baseline','candidate') } else { @('candidate','baseline') }
            foreach ($label in $labels) {
                $exe = if ($label -eq 'baseline') { $BaselineHarness } else { $CandidateHarness }
                Invoke-Harness $exe $test (Join-Path $components "$label-$($test -replace '::','-').txt")
            }
        }
        Invoke-Harness $CandidateHarness 'git::operations::tests::benchmark_tags' (Join-Path $components 'candidate-tags.txt')
    }
    Assert-Idle
    & "$PSScriptRoot/summarize-tags-toolbar.ps1" -OutputDirectory $OutputDirectory
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
