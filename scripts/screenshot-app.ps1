param(
    [string]$Candidate = "$PSScriptRoot/../target/release/git-rustle.exe",
    [ValidateSet('graph','branches','settings','diff','stash-panel','remote-checkout','detached-commit','graph-selection-boundary','ghost-files-github','ghost-files-gitlab','request-branches','request-branches-menu','graph-request-menu','graph-request-compose','graph-new-branch','graph-new-branch-review','graph-reset','graph-reset-review','graph-delete','graph-delete-review','graph-branch-menu','graph-branch-menu-edge','graph-polish','graph-polish-detached','ai-settings','ai-commit','ai-summary','ai-rebase','ai-rebase-cancel','ai-rebase-error','ai-merge','ai-merge-cancel','ai-merge-error','ai-conflict','ai-conflict-rebase','ai-conflict-pull','ai-stale','ai-cancel','ai-error')]
    [string[]]$Scenarios = @('graph','branches','settings','diff','stash-panel'),
    [string]$OutputDirectory = "$PSScriptRoot/../target/bench/screenshots-$([DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss'))-$([Guid]::NewGuid().ToString('N').Substring(0,8))",
    [ValidateRange(10,120)][int]$TimeoutSeconds = 30,
    [ValidateSet('Codex','Claude')][string]$AiProvider = 'Codex',
    [ValidateSet('1440x900','900x600')][string]$WindowSize = '1440x900',
    [switch]$LegacyClaude,
    [switch]$DisableDirectComposition
)
$ErrorActionPreference = 'Stop'
if ($LegacyClaude -and $AiProvider -ne 'Claude') { throw 'LegacyClaude requires the Claude fixture provider' }
if($Scenarios -contains 'remote-checkout'){
    if($Scenarios.Count-ne1){throw 'Capture remote checkout separately'}
    & "$PSScriptRoot/benchmark-remote-checkout.ps1" -Candidate $Candidate -Screenshots -OutputDirectory $OutputDirectory
    return
}
if(@($Scenarios|Where-Object{$_-in@('detached-commit','graph-selection-boundary','ghost-files-github','ghost-files-gitlab','graph-request-menu','graph-request-compose','graph-new-branch','graph-new-branch-review','graph-reset','graph-reset-review','graph-delete','graph-delete-review','graph-branch-menu','graph-branch-menu-edge','graph-polish','graph-polish-detached')}).Count){
    if(@($Scenarios|Where-Object{$_-notin@('detached-commit','graph-selection-boundary','ghost-files-github','ghost-files-gitlab','graph-request-menu','graph-request-compose','graph-new-branch','graph-new-branch-review','graph-reset','graph-reset-review','graph-delete','graph-delete-review','graph-branch-menu','graph-branch-menu-edge','graph-polish','graph-polish-detached')}).Count){throw 'Capture graph harness scenarios separately from other views'}
    & "$PSScriptRoot/benchmark-graph-features.ps1" -Candidate $Candidate -Scenarios $Scenarios -Screenshots -DisableDirectComposition:$DisableDirectComposition -WindowSize $WindowSize -OutputDirectory $OutputDirectory
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
    'PATH','LOCALAPPDATA','APPDATA','GPUI_DISABLE_DIRECT_COMPOSITION','GIT_CONFIG_NOSYSTEM','GIT_CONFIG_GLOBAL','GIT_TERMINAL_PROMPT',
    'GIT_AUTHOR_DATE','GIT_COMMITTER_DATE','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP','GIT_RUSTLE_AI_FIXTURE_LOG','GIT_RUSTLE_AI_FIXTURE_LEGACY'
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
        index = (Invoke-FixtureGit $Repository @('ls-files','--stage'))
        stash = (Invoke-FixtureGit $Repository @('stash','list','--format=%H'))
        files = $files
    } | ConvertTo-Json -Depth 5 -Compress
}
try {
    foreach ($name in $names | Where-Object { $_ -ne 'PATH' }) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    if (@($Scenarios | Where-Object { $_ -like 'ai-*' }).Count) {
        $bin = Join-Path $OutputDirectory 'fixture-bin'
        New-Item -ItemType Directory -Path $bin | Out-Null
        $fixtureExe = Join-Path $bin 'codex.exe'
        & rustc --edition=2024 "$PSScriptRoot/ai-cli-fixture.rs" -o $fixtureExe
        if ($LASTEXITCODE -ne 0) { throw 'Could not compile deterministic AI CLI fixture' }
        Copy-Item -LiteralPath $fixtureExe -Destination (Join-Path $bin 'claude.exe')
        $env:PATH = "$bin;$env:PATH"
    }
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
        if ($scenario -like 'ai-*') {
            [IO.File]::WriteAllText((Join-Path $fixture '.git/git-rustle-ai-fixture'),'disposable AI fixture')
            $settingsDir = Join-Path $env:LOCALAPPDATA 'git-rustle'
            New-Item -ItemType Directory -Path $settingsDir | Out-Null
            # Omitted prompts use the production defaults, proving backward-compatible loading.
            @{ provider = $AiProvider; model = 'fixture-model' } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $settingsDir 'ai.json')
            [IO.File]::WriteAllText((Join-Path $settingsDir 'tools.json'),'{"scanned":true,"tools":[]}')
            $env:GIT_RUSTLE_AI_FIXTURE_LOG = Join-Path $directory 'ai-context.txt'
            if ($LegacyClaude) { $env:GIT_RUSTLE_AI_FIXTURE_LEGACY = '1' }
            if ($scenario -eq 'ai-summary') {
                # A real patch larger than the former 256 KiB cap must reach the CLI intact.
                $summaryNotes = "Untracked fixture notes`n" + ((0..9999 | ForEach-Object { "Expanded AI summary fixture line $_ with relevant changes`n" }) -join '')
                [IO.File]::WriteAllText((Join-Path $fixture 'notes.txt'),$summaryNotes)
            }
            if ($scenario -like 'ai-rebase*' -or $scenario -like 'ai-merge*') {
                [IO.File]::AppendAllText((Join-Path $fixture '.git/info/exclude'),"`nnotes.txt`n")
                Invoke-FixtureGit $fixture @('restore','README.md') | Out-Null
                Invoke-FixtureGit $fixture @('switch','-c','ai-destination') | Out-Null
                foreach ($name in 'example.rs','README.md') { [IO.File]::WriteAllText((Join-Path $fixture $name),"destination change`n") }
                Invoke-FixtureGit $fixture @('commit','-am','Destination change') | Out-Null
                Invoke-FixtureGit $fixture @('switch','main') | Out-Null
                foreach ($n in 'first','second') {
                    foreach ($name in 'example.rs','README.md') { [IO.File]::WriteAllText((Join-Path $fixture $name),"local $n`n") }
                    Invoke-FixtureGit $fixture @('commit','-am',"Local $n") | Out-Null
                }
            }
            if ($scenario -like 'ai-conflict*') {
                Invoke-FixtureGit $fixture @('restore','README.md') | Out-Null
                Invoke-FixtureGit $fixture @('switch','-c','ai-incoming') | Out-Null
                [IO.File]::WriteAllText((Join-Path $fixture 'example.rs'),"incoming change`n")
                Invoke-FixtureGit $fixture @('commit','-am','Incoming change') | Out-Null
                Invoke-FixtureGit $fixture @('switch','main') | Out-Null
                [IO.File]::WriteAllText((Join-Path $fixture 'example.rs'),"local change`n")
                Invoke-FixtureGit $fixture @('commit','-am','Local change') | Out-Null
                if ($scenario -eq 'ai-conflict-rebase') { & git -C $fixture rebase ai-incoming 2>&1 | Out-Null }
                elseif ($scenario -eq 'ai-conflict-pull') { & git -C $fixture pull --no-rebase . ai-incoming 2>&1 | Out-Null }
                else { & git -C $fixture merge --no-edit ai-incoming 2>&1 | Out-Null }
                if ($LASTEXITCODE -ne 1) { throw 'Expected a disposable merge/rebase/pull conflict' }
            }
        }
        $before = Get-FixtureFingerprint $fixture
        $before | Set-Content -LiteralPath (Join-Path $directory 'fixture-before.json')
        $env:GIT_RUSTLE_TIMING = '1'; $env:GIT_RUSTLE_TRACE = '1'
        # The capture worker requests normal application close after the screenshot.
        $env:GIT_RUSTLE_AUTOQUIT = '600000'
        $env:GIT_RUSTLE_BENCH_WINDOW = $WindowSize
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
            $integration = $scenario -like 'ai-rebase*' -or $scenario -like 'ai-merge*'
            $merging = $scenario -like 'ai-merge*'
            foreach ($key in $(if ($integration) { @('stash') } else { @('head','refs','status','stash','index') })) {
                if ($beforeState.$key -cne $afterState.$key) { throw "Visual scenario unexpectedly changed fixture $key" }
            }
            if ($beforeState.files.Count -ne $afterState.files.Count) { throw 'Visual scenario unexpectedly changed fixture file count' }
            for ($i = 0; $i -lt $beforeState.files.Count; $i++) {
                if ($integration -and $beforeState.files[$i].name -in @('example.rs','README.md')) { continue }
                if ($beforeState.files[$i].name -cne $afterState.files[$i].name -or $beforeState.files[$i].sha256 -cne $afterState.files[$i].sha256) { throw 'Visual scenario unexpectedly changed fixture files' }
            }
            $result.fixtureUnchanged = !$integration
            if ($integration) {
                $pausedState = if ($merging) { '.git/MERGE_HEAD' } else { '.git/rebase-merge' }
                $unmerged = Invoke-FixtureGit $fixture @('ls-files','-u')
                if ($scenario -in @('ai-rebase','ai-merge')) {
                    $branch = Invoke-FixtureGit $fixture @('symbolic-ref','--short','HEAD')
                    if ($unmerged -or $branch -cne 'main' -or $afterState.head -ceq $beforeState.head) { throw 'AI integration did not finish on the current branch' }
                    if ($afterState.status -cne $beforeState.status) { throw 'AI integration left unexpected pending changes' }
                    foreach ($name in 'example.rs','README.md') {
                        $expected = if ($merging) { "resolved AI merge fixture`n" } else { "resolved AI fixture second`n" }
                        if ([IO.File]::ReadAllText((Join-Path $fixture $name)) -cne $expected) { throw 'AI did not resolve every conflicted file' }
                    }
                    Invoke-FixtureGit $fixture @('merge-base','--is-ancestor','ai-destination','HEAD') | Out-Null
                    if ($merging) {
                        $parents = (Invoke-FixtureGit $fixture @('rev-list','--parents','-n','1','HEAD')) -split ' '
                        $incoming = Invoke-FixtureGit $fixture @('rev-parse','ai-destination')
                        if ($parents.Count -ne 3 -or $parents[1] -cne $beforeState.head -or $parents[2] -cne $incoming) { throw 'AI merge did not preserve both parents' }
                    }
                    if (Test-Path -LiteralPath (Join-Path $fixture $pausedState)) { throw 'AI integration state was not cleared' }
                } else {
                    if (!$unmerged -or !(Test-Path -LiteralPath (Join-Path $fixture $pausedState))) { throw 'Cancelled/failed AI integration did not retain its paused state' }
                }
                foreach ($line in ($beforeState.refs -split "`n")) {
                    if ($scenario -in @('ai-rebase','ai-merge') -and $line.EndsWith(' refs/heads/main')) { continue }
                    if ($line -cnotin ($afterState.refs -split "`n")) { throw 'AI integration changed an unrelated branch or tag' }
                }
                $result['integrationPostconditionsVerified'] = $true
            }
            if ($scenario -like 'ai-*') {
                if ((Get-Content -LiteralPath $log -Raw) -notmatch 'AI scenario verified after native paint') { throw 'AI actions were not verified after native paint' }
                if ($scenario -eq 'ai-settings') {
                    $ai = Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'git-rustle/ai.json') -Raw | ConvertFrom-Json
                    if ($ai.provider -cne $AiProvider -or $ai.model -cne 'fixture-model' -or $ai.prompts.Count -ne 5 -or $ai.prompts[2] -notmatch 'behavior') { throw 'Global AI settings were not persisted correctly' }
                } elseif ($scenario -ne 'ai-cancel' -and !(Test-Path -LiteralPath $env:GIT_RUSTLE_AI_FIXTURE_LOG)) { throw 'AI request did not invoke the deterministic CLI' }
                if ($AiProvider -eq 'Claude' -and $scenario -ne 'ai-settings') {
                    $invocations=[IO.File]::ReadAllLines([IO.Path]::ChangeExtension($env:GIT_RUSTLE_AI_FIXTURE_LOG,'invocations.txt'))
                    if ($LegacyClaude) {
                        if ($invocations.Count -lt 2 -or $invocations[0] -cne 'safe-mode=true' -or @($invocations | Select-Object -Skip 1 | Where-Object { $_ -cne 'safe-mode=false' }).Count) { throw 'Legacy Claude did not fall back once and remember the unsupported flag' }
                        if ($scenario -eq 'ai-merge' -and $invocations.Count -ne 3) { throw 'Legacy Claude repeated the unsupported flag probe for another conflict file' }
                    } elseif (@($invocations | Where-Object { $_ -cne 'safe-mode=true' }).Count) { throw 'Supported Claude did not use safe mode' }
                    $result['claudeSafeModeVerified']=$true
                }
            }
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
        legacyClaudeFixture = [bool]$LegacyClaude
        scenarios = @($results.ToArray())
        limitations = 'Inspect PNGs for appearance. Capture sanity checks cannot prove complete rendering or interaction feel. These runs are excluded from performance comparisons.'
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.json') -Encoding utf8
}
