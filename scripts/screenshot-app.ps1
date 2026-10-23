param(
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateSet('graph','branches','settings','diff','stash-panel','graph-selection-boundary','request-branches','request-branches-menu','graph-request-menu','graph-request-compose')]
    [string[]]$Scenarios = @('graph','branches','settings','diff','stash-panel'),
    [string]$OutputDirectory = "$PSScriptRoot/../target/bench/screenshots-$([DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss'))-$([Guid]::NewGuid().ToString('N').Substring(0,8))",
    [ValidateRange(10,120)][int]$TimeoutSeconds = 30,
    [switch]$DisableDirectComposition
)
$ErrorActionPreference = 'Stop'
if(@($Scenarios|Where-Object{$_-in@('graph-selection-boundary','graph-request-menu','graph-request-compose')}).Count){
    if(@($Scenarios|Where-Object{$_-notin@('graph-selection-boundary','graph-request-menu','graph-request-compose')}).Count){throw 'Capture graph harness scenarios separately from other views'}
    & "$PSScriptRoot/benchmark-graph-features.ps1" -Candidate $Candidate -Scenarios $Scenarios -Screenshots -DisableDirectComposition:$DisableDirectComposition -OutputDirectory $OutputDirectory
    return
}
. "$PSScriptRoot/benchmark-screenshot.ps1"
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw "Retaining existing screenshot run: $OutputDirectory" }
if (!$Scenarios.Count -or @($Scenarios | Select-Object -Unique).Count -ne $Scenarios.Count) { throw 'Choose at least one scenario, without duplicates' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$saved = @{}
$names = @(@(Get-ChildItem Env: | Where-Object { $_.Name -match '^(GIT_|GIT_RUSTLE_)' } | ForEach-Object Name) + @(
    'LOCALAPPDATA','APPDATA','GPUI_DISABLE_DIRECT_COMPOSITION','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_GLOBAL','GIT_TERMINAL_PROMPT',
    'GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP'
) | Select-Object -Unique)
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$results = [Collections.Generic.List[object]]::new()
$failure = $null
function Invoke-FixtureGit([string]$Repository,[string[]]$Arguments) {
    $output = @(& git -C $Repository @Arguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "Screenshot fixture Git failed: $($Arguments -join ' '): $($output -join ' ')" }
    return ($output -join "`n")
}
function Get-FixtureFingerprint([string]$Repository) {
    $files = @(Get-ChildItem -LiteralPath $Repository -File | Sort-Object Name | ForEach-Object {
        @{ name = $_.Name; sha256 = (Get-FileHash -LiteralPath $_.FullName).Hash }
    })
    @{
        head = (Invoke-FixtureGit $Repository @('rev-parse','HEAD'))
        refs = (Invoke-FixtureGit $Repository @('show-ref'))
        status = (Invoke-FixtureGit $Repository @('status','--porcelain=v1'))
        stash = (Invoke-FixtureGit $Repository @('stash','list','--format=%H'))
        files = $files
    } | ConvertTo-Json -Depth 5 -Compress
}
try {
    foreach ($name in $names) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    $env:GIT_CONFIG_NOSYSTEM = '1'
    if ($DisableDirectComposition) { $env:GPUI_DISABLE_DIRECT_COMPOSITION = '1' }
    $env:GIT_CONFIG_GLOBAL = Join-Path $OutputDirectory 'empty-gitconfig'
    [IO.File]::WriteAllText($env:GIT_CONFIG_GLOBAL,'')
    $env:GIT_TERMINAL_PROMPT = '0'
    foreach ($scenario in $Scenarios) {
        $directory = Join-Path $OutputDirectory $scenario
        $fixture = Join-Path $directory 'fixture'
        New-Item -ItemType Directory -Path $fixture -Force | Out-Null
        $env:LOCALAPPDATA = Join-Path $directory 'appdata'
        $env:APPDATA = Join-Path $directory 'roaming'
        New-Item -ItemType Directory -Path $env:LOCALAPPDATA,$env:APPDATA | Out-Null
        Invoke-FixtureGit $fixture @('init','-b','main') | Out-Null
        foreach ($pair in @(@('user.name','Screenshot Fixture'),@('user.email','screenshots@example.invalid'),@('commit.gpgsign','false'),@('core.autocrlf','false'),@('core.hooksPath','disabled-fixture-hooks'))) {
            Invoke-FixtureGit $fixture (@('config') + $pair) | Out-Null
        }
        [IO.File]::WriteAllText((Join-Path $fixture '.git/git-rustle-todos-fixture'),'disposable todos benchmark fixture')
        foreach ($n in 1..6) {
            $env:GIT_AUTHOR_DATE = $env:GIT_COMMITTER_DATE = "2026-01-01T12:00:0${n}Z"
            [IO.File]::WriteAllText((Join-Path $fixture 'example.rs'),("fn main() {{`n    println!(`"Screenshot fixture revision {0}`");`n}}`n" -f $n))
            [IO.File]::WriteAllText((Join-Path $fixture 'README.md'),"# Screenshot fixture`n`nRevision $n for isolated visual review.`n")
            Invoke-FixtureGit $fixture @('add','--all') | Out-Null
            Invoke-FixtureGit $fixture @('commit','-m',"Improve fixture revision $n") | Out-Null
        }
        Invoke-FixtureGit $fixture @('branch','feature/visual-review','HEAD~2') | Out-Null
        Invoke-FixtureGit $fixture @('tag','v0.1.0','HEAD~3') | Out-Null
        if ($scenario -in @('request-branches','request-branches-menu')) {
            $mainBase = (Invoke-FixtureGit $fixture @('rev-parse','HEAD~5')).Trim()
            foreach ($branch in @(@('Dev','HEAD~1'),@('FeatureA','HEAD'))) {
                Invoke-FixtureGit $fixture (@('branch') + $branch) | Out-Null
            }
            Invoke-FixtureGit $fixture @('symbolic-ref','HEAD','refs/heads/FeatureA') | Out-Null
            Invoke-FixtureGit $fixture @('branch','-D','main','feature/visual-review') | Out-Null
            Invoke-FixtureGit $fixture @('branch','Main',$mainBase) | Out-Null
            foreach ($branch in @('Main','Dev','FeatureA')) {
                Invoke-FixtureGit $fixture @('update-ref',"refs/remotes/origin/$branch","refs/heads/$branch") | Out-Null
            }
            Invoke-FixtureGit $fixture @('update-ref','refs/remotes/origin/remote-only','refs/heads/Main') | Out-Null
            Invoke-FixtureGit $fixture @('update-ref','refs/remotes/other/wrong-project','refs/heads/Main') | Out-Null
            Invoke-FixtureGit $fixture @('symbolic-ref','refs/remotes/origin/HEAD','refs/remotes/origin/Main') | Out-Null
            [IO.File]::WriteAllText((Join-Path $fixture '.git/git-rustle-request-branches-fixture'),'disposable request branch fixture')
        }
        foreach ($n in 1..3) {
            [IO.File]::AppendAllText((Join-Path $fixture 'README.md'),"`nSaved draft $n`n")
            Invoke-FixtureGit $fixture @('stash','push','-m',"Visual review draft $n") | Out-Null
        }
        [IO.File]::AppendAllText((Join-Path $fixture 'README.md'),"`nPending documentation update`n")
        [IO.File]::WriteAllText((Join-Path $fixture 'notes.txt'),"Untracked fixture notes`n")
        $before = Get-FixtureFingerprint $fixture
        $before | Set-Content -LiteralPath (Join-Path $directory 'fixture-before.json')
        $env:GIT_RUSTLE_TIMING = '1'; $env:GIT_RUSTLE_TRACE = '1'
        # The capture worker requests normal application close after the screenshot.
        $env:GIT_RUSTLE_AUTOQUIT = '600000'
        if ($scenario -eq 'graph') { Remove-Item Env:GIT_RUSTLE_SCENARIO -ErrorAction SilentlyContinue }
        else { $env:GIT_RUSTLE_SCENARIO = $scenario }
        $log = Join-Path $directory 'app.log'
        $process = $null
        $result = [ordered]@{ scenario = $scenario; outputDirectory = $directory; capture = $null; exitCode = $null; cleanupVerified = $false; fixtureUnchanged = $false }
        try {
            $process = Start-BenchmarkNativeProcess -FilePath $Candidate -ArgumentList @(('"' + $fixture + '"'),'--no-cache') -RedirectStandardError $log
            $deadline = [Diagnostics.Stopwatch]::StartNew()
            $readyPattern = if ($scenario -eq 'graph') { '\bfirst frame \(' } else { '\bdesktop scenario frame\b' }
            do {
                if ($process.WaitForExit(0)) { throw "Application exited before screenshot readiness; see $log" }
                $text = if (Test-Path -LiteralPath $log) { Get-Content -LiteralPath $log -Raw } else { '' }
                if ($text -match $readyPattern) {
                    @{
                        scenario = $scenario; observedUtc = [DateTime]::UtcNow.ToString('o')
                        paintMark = @($text -split "`r?`n" | Where-Object { $_ -match $readyPattern })[-1]
                        source = 'Application trace after GPUI paint callback; not a full model snapshot'
                    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $directory 'readiness.json')
                    break
                }
                if ($deadline.Elapsed.TotalSeconds -ge $TimeoutSeconds) { throw "Timed out waiting for verified $scenario paint; see $log" }
                Start-Sleep -Milliseconds 50
            } while ($true)
            $result.capture = Save-BenchmarkNativeScreenshot -Process $process -OutputPath (Join-Path $directory 'screenshot.png') -CloseAfterCapture
            if (!$process.WaitForExit(10000)) { throw "Captured app did not close normally; see $log" }
            $result.exitCode = $process.ExitCode
            if ($result.exitCode -ne 0) { throw "Captured application exited with $($result.exitCode); see $log" }
            $after = Get-FixtureFingerprint $fixture
            $after | Set-Content -LiteralPath (Join-Path $directory 'fixture-after.json')
            # Compare parsed values: JSON hashtable key order is not significant.
            $beforeState = $before | ConvertFrom-Json; $afterState = $after | ConvertFrom-Json
            foreach ($key in 'head','refs','status','stash') {
                if ($beforeState.$key -cne $afterState.$key) { throw "Visual scenario unexpectedly changed fixture $key" }
            }
            if ($beforeState.files.Count -ne $afterState.files.Count) { throw 'Visual scenario unexpectedly changed fixture file count' }
            for ($i = 0; $i -lt $beforeState.files.Count; $i++) {
                if ($beforeState.files[$i].name -cne $afterState.files[$i].name -or $beforeState.files[$i].sha256 -cne $afterState.files[$i].sha256) { throw 'Visual scenario unexpectedly changed fixture files' }
            }
            $result.fixtureUnchanged = $true
        } finally {
            if ($null -ne $process) {
                $process.Dispose()
                $result.cleanupVerified = $process.WaitForExit(0)
            }
            $results.Add([pscustomobject]$result)
        }
        Write-Output "Captured $scenario -> $directory/screenshot.png"
    }
} catch {
    $failure = $_.ToString()
    throw
} finally {
    foreach ($name in $names) {
        if ($null -eq $saved[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    }
    @{
        schemaVersion = 1; success = ($null -eq $failure); error = $failure
        candidate = $Candidate; sha256 = (Get-FileHash -LiteralPath $Candidate).Hash
        directCompositionDisabled = [bool]$DisableDirectComposition
        scenarios = @($results.ToArray())
        limitations = 'Inspect PNGs for appearance. Capture sanity checks cannot prove complete rendering or interaction feel. These runs are excluded from performance comparisons.'
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.json') -Encoding utf8
}
