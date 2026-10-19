param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Actions','Components','Preview')]
    [string[]]$Stages = @('Startup','History','Actions','Components','Preview'),
    [string[]]$ComponentTests = @('remote::history::tests::benchmark_tag_refresh','remote::requests::integration::tests::benchmark_provider_cache','desktop::provider::tests::benchmark_provider_lookup'),
    [string[]]$ActionScenarios = @('search-focus','branch-search-focus','search-clear','search-clear-multi','provider-scroll','provider-menu')
)
# Build/preserve executables first. Every stage runs sequentially; no Cargo command
# can accidentally turn a component measurement into a compilation measurement.
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$CandidateHarness = (Resolve-Path -LiteralPath $CandidateHarness).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('LOCALAPPDATA','GIT_RUSTLE_GHOST_FIXTURE','GIT_RUSTLE_BENCH_REPO','GIT_RUSTLE_SCENARIO')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Assert-Idle {
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) {
        throw 'Timing rejected: finish every build before starting this suite'
    }
}
function New-Stage([string]$Name) {
    $path = Join-Path $OutputDirectory $Name
    if (Test-Path -LiteralPath $path) { throw "Retaining existing results; choose a new output directory or omit stage $Name" }
    New-Item -ItemType Directory $path | Out-Null
    return $path
}
function Invoke-Harness([string]$Name,[string]$Log) {
    Assert-Idle
    & $CandidateHarness $Name --ignored --exact --nocapture 2>&1 | Tee-Object -FilePath $Log
    if ($LASTEXITCODE -ne 0) { throw "Harness failed: $Name (retained $Log)" }
    Assert-Idle
    $text = Get-Content -LiteralPath $Log -Raw
    if ($text -notmatch '1 passed') { throw "Harness did not execute exactly one successful test: $Name" }
}
try {
    Assert-Idle
    # Keep fixture, identity and image cache writes inside the ignored run folder.
    $env:LOCALAPPDATA = Join-Path $OutputDirectory 'app-data'
    New-Item -ItemType Directory -Force $env:LOCALAPPDATA | Out-Null
    $env:GIT_RUSTLE_BENCH_REPO = $Repository
    Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    $sourceHead = & git -C "$PSScriptRoot/.." rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot record source revision' }
    $sourceStatus = @(& git -C "$PSScriptRoot/.." status --porcelain=v1)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot record source status' }
    $provenance = [pscustomobject]@{
        recordedUtc = [DateTime]::UtcNow.ToString('o')
        repository = $Repository
        baseline = $Baseline; baselineSha256 = (Get-FileHash -LiteralPath $Baseline).Hash; baselineBytes = (Get-Item -LiteralPath $Baseline).Length
        candidate = $Candidate; candidateSha256 = (Get-FileHash -LiteralPath $Candidate).Hash; candidateBytes = (Get-Item -LiteralPath $Candidate).Length
        harness = $CandidateHarness; harnessSha256 = (Get-FileHash -LiteralPath $CandidateHarness).Hash
        sourceHead = $sourceHead; sourceStatus = $sourceStatus
        machine = [Environment]::MachineName
        os = [Environment]::OSVersion.ToString()
        logicalProcessors = [Environment]::ProcessorCount
        samples = $Samples; stages = $Stages; componentTests = $ComponentTests
        cache = $env:LOCALAPPDATA
    }
    $provenance | ConvertTo-Json -Depth 4 | Set-Content (Join-Path $OutputDirectory "provenance-$([Guid]::NewGuid().ToString('N')).json")
    if ('Startup' -in $Stages) {
        & "$PSScriptRoot/benchmark.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate `
            -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'startup')
    }
    if ('History' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate `
            -Scenarios @('scroll','search-scroll','branches','search','status-panels') `
            -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'history')
    }
    if ('Actions' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate `
            -Scenarios $ActionScenarios -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'actions')
    }
    if ('Components' -in $Stages) {
        $components = New-Stage 'components'
        foreach ($test in $ComponentTests) {
            Invoke-Harness $test (Join-Path $components (($test -replace '::','-') + '.txt'))
        }
        & "$PSScriptRoot/benchmark-provider-fixture.ps1" -CandidateHarness $CandidateHarness `
            -Samples $Samples -OutputDirectory (Join-Path $components 'provider-fixture')
    }
    if ('Preview' -in $Stages) {
        $preview = New-Stage 'preview'
        $env:GIT_RUSTLE_GHOST_FIXTURE = Join-Path $preview 'repository'
        Invoke-Harness 'remote::history::tests::prepare_native_fixture' (Join-Path $preview 'fixture.txt')
        # Generate this only now: the production branch/tag cache TTL is five minutes.
        & "$PSScriptRoot/benchmark-remote-preview.ps1" -Repository $env:GIT_RUSTLE_GHOST_FIXTURE `
            -Baseline $Baseline -Candidate $Candidate -NativeBaseline -NativeCandidate `
            -RejectCompetingBuilds -Samples $Samples -OutputDirectory $preview
    }
    Assert-Idle
    & "$PSScriptRoot/summarize-app-improvements.ps1" -OutputDirectory $OutputDirectory
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
