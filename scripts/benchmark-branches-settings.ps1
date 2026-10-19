param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$BaselineHarness,
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateRange(1,100)][int]$Samples = 15,
    [ValidateSet('Startup','History','Actions','Components')]
    [string[]]$Stages = @('Startup','History','Actions','Components'),
    [string[]]$ActionScenarios = @('filter-visuals','general-settings','general-tabs','provider-scroll','provider-menu'),
    [string[]]$CandidateComponentTests = @('branches::tests::benchmark_owner_filter','remote::requests::integration::tests::benchmark_gitlab_project_icon'),
    [string[]]$SharedComponentTests = @('branches::tests::benchmark_browser','remote::requests::integration::tests::benchmark_provider_cache','desktop::provider::tests::benchmark_provider_lookup','update::tests::benchmark_check')
)
$ErrorActionPreference = 'Stop'
foreach ($name in @('Repository','Baseline','BaselineHarness','CandidateHarness','Candidate')) {
    Set-Variable -Name $name -Value (Resolve-Path -LiteralPath (Get-Variable $name -ValueOnly)).Path
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('LOCALAPPDATA','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_BENCH_REPO','GIT_RUSTLE_BENCH_BRANCHES')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Assert-Idle {
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Finish builds before timing.' }
}
function New-Stage([string]$Name) {
    $path = Join-Path $OutputDirectory $Name
    if (Test-Path -LiteralPath $path) { throw "Retaining existing $Name results; choose a new output directory or omit this stage." }
    New-Item -ItemType Directory $path | Out-Null
    return $path
}
function Invoke-Harness([string]$Exe,[string]$Test,[string]$Log) {
    Assert-Idle
    & $Exe $Test --ignored --exact --nocapture 2>&1 | Tee-Object -FilePath $Log
    if ($LASTEXITCODE -ne 0) { throw "Failed $Test; retained $Log" }
    Assert-Idle
    $text = Get-Content -LiteralPath $Log -Raw
    if ($text -notmatch 'running 1 test' -or $text -notmatch 'test result: ok\. 1 passed;') { throw "Expected exactly one successful test: $Log" }
}
try {
    Assert-Idle
    $env:LOCALAPPDATA = Join-Path $OutputDirectory 'app-data'
    New-Item -ItemType Directory -Force $env:LOCALAPPDATA | Out-Null
    [IO.File]::WriteAllText((Join-Path $env:LOCALAPPDATA 'cache-benchmark-fixture'),'disposable image cache benchmark')
    $env:GIT_RUSTLE_BENCH_REPO = $Repository
    $env:GIT_RUSTLE_BENCH_BRANCHES = '1000'
    Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    $executables = foreach ($name in @('Baseline','BaselineHarness','Candidate','CandidateHarness')) {
        $path = Get-Variable $name -ValueOnly
        [pscustomobject]@{role=$name;path=$path;bytes=(Get-Item -LiteralPath $path).Length;sha256=(Get-FileHash -LiteralPath $path).Hash}
    }
    [pscustomobject]@{
        recordedUtc=[DateTime]::UtcNow.ToString('o');repository=$Repository;executables=$executables
        sourceHead=(& git -C "$PSScriptRoot/.." rev-parse HEAD);sourceStatus=@(& git -C "$PSScriptRoot/.." status --porcelain=v1)
        samples=$Samples;stages=$Stages;actionScenarios=$ActionScenarios
        sharedComponents=$SharedComponentTests;candidateComponents=$CandidateComponentTests
        cache=$env:LOCALAPPDATA;os=[Environment]::OSVersion.ToString();logicalProcessors=[Environment]::ProcessorCount
        componentOrder='Separate warmed binary blocks; baseline/candidate order reverses between shared harnesses. Native shared launches alternate order.'
    } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $OutputDirectory "provenance-$([Guid]::NewGuid().ToString('N')).json")
    if ('Startup' -in $Stages) {
        # Keep repeated launches local. Manual update HTTP is measured separately
        # against the controlled fixture, rather than exhausting the public API.
        $env:GIT_RUSTLE_SCENARIO = 'startup-local'
        & "$PSScriptRoot/benchmark.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -NativeBaseline -NativeCandidate -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'startup')
        Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue
    }
    if ('History' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Baseline $Baseline -Candidate $Candidate -Scenarios @('scroll','search-scroll','branches','search','settings-tabs') -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'history')
    }
    if ('Actions' -in $Stages) {
        & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $Repository -Candidate $Candidate -Scenarios $ActionScenarios -RejectCompetingBuilds -Samples $Samples -OutputDirectory (New-Stage 'actions')
    }
    if ('Components' -in $Stages) {
        $components = New-Stage 'components'
        for ($i=0; $i -lt $SharedComponentTests.Count; $i++) {
            $test = $SharedComponentTests[$i]
            $roles = if ($i % 2 -eq 0) { @('baseline','candidate') } else { @('candidate','baseline') }
            foreach ($role in $roles) {
                $exe = if ($role -eq 'baseline') { $BaselineHarness } else { $CandidateHarness }
                Invoke-Harness $exe $test (Join-Path $components "$role-$($test -replace '::','-').txt")
            }
        }
        foreach ($test in $CandidateComponentTests) {
            Invoke-Harness $CandidateHarness $test (Join-Path $components "candidate-$($test -replace '::','-').txt")
        }
    }
    Assert-Idle
    & "$PSScriptRoot/summarize-branches-settings.ps1" -OutputDirectory $OutputDirectory
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
