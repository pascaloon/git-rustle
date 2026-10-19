param(
    [Parameter(Mandatory)][string]$CandidateHarness,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(1,100)][int]$Samples = 15
)
# This intentionally measures the complete fixture-test process, rather than
# claiming an isolated HTTP interval from a functional regression test.
$ErrorActionPreference = 'Stop'
$CandidateHarness = (Resolve-Path -LiteralPath $CandidateHarness).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a new output directory to retain all prior samples' }
New-Item -ItemType Directory $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$test = 'remote::requests::integration::tests::provider_pagination_project_icons_and_offline_cache_reuse'
$saved = @{}
foreach ($name in @('LOCALAPPDATA','TEMP','TMP','TMPDIR')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Assert-Idle {
    if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) {
        throw 'Timing rejected: competing build detected'
    }
}
function Run-One([int]$Sample) {
    Assert-Idle
    $stdout = Join-Path $OutputDirectory "sample-$Sample-stdout.txt"
    $stderr = Join-Path $OutputDirectory "sample-$Sample-stderr.txt"
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process -FilePath $CandidateHarness -ArgumentList @($test,'--exact','--nocapture') `
        -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru
    if (!$process.WaitForExit(30000)) {
        Stop-Process -Id $process.Id
        throw "Fixture process timeout; retained $stdout and $stderr"
    }
    $process.Refresh()
    $clock.Stop()
    Assert-Idle
    if ($process.ExitCode -ne 0) { throw "Fixture failed; retained $stdout and $stderr" }
    $text = Get-Content -LiteralPath $stdout -Raw
    if ($text -notmatch 'running 1 test' -or $text -notmatch 'test result: ok\. 1 passed;') {
        throw "Fixture did not execute exactly one successful test: $stdout"
    }
    return [pscustomobject]@{ sample=$Sample; elapsedMs=$clock.Elapsed.TotalMilliseconds; test=$test }
}
try {
    Assert-Idle
    $env:LOCALAPPDATA = Join-Path $OutputDirectory 'app-data'
    $temporary = Join-Path $OutputDirectory 'temporary'
    New-Item -ItemType Directory $env:LOCALAPPDATA,$temporary | Out-Null
    foreach ($name in @('TEMP','TMP','TMPDIR')) { [Environment]::SetEnvironmentVariable($name,$temporary,'Process') }
    [pscustomobject]@{
        harness=$CandidateHarness; sha256=(Get-FileHash -LiteralPath $CandidateHarness).Hash
        test=$test; warmups=1; samples=$Samples; recordedUtc=[DateTime]::UtcNow.ToString('o')
        boundary='PowerShell process launch through exit/refresh; includes fixture construction, both providers, loopback HTTP, paging, project metadata, cache persistence/offline reuse, assertions and redirected log I/O; excludes parent output validation'
    } | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'provenance.json')
    Run-One -1 | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'warmup.json')
    $results = @()
    for ($i=0; $i -lt $Samples; $i++) {
        $results += Run-One $i
        $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    }
    $results | Format-Table sample,elapsedMs
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
