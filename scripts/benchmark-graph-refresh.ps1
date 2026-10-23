[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Baseline,
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Python,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(1,100)][int]$Samples = 15,
    [switch]$Capture,
    [switch]$EnableCache,
    [switch]$BaselineSmart,
    [switch]$CandidateOnly,
    [ValidateSet('changed','unchanged','full','after-branches')][string]$RefreshCase = 'changed',
    [ValidateRange(0,10000)][int]$ServerOnlyBranches = 1000,
    [ValidateRange(2,10000)][int]$Branches = 12,
    [ValidateRange(0,10000)][int]$Tags = 0,
    [switch]$UnrelatedFirst,
    [switch]$ApiOnly,
    [switch]$GitCliOnly,
    [switch]$TrackingOnlyGhost,
    [switch]$MalformedAdvertisement,
    [switch]$UnicodeTagAdvertisement,
    [ValidateRange(0,1000)][int]$DelayMs = 60
)
$ErrorActionPreference = 'Stop'
if ($MalformedAdvertisement -and (!$CandidateOnly -or $ApiOnly -or $GitCliOnly -or $Branches -lt 64)) {
    throw 'MalformedAdvertisement requires CandidateOnly, at least 64 branches, and no transport rejection flags.'
}
if ($RefreshCase -ne 'changed' -and !$CandidateOnly) { throw 'New scenarios require -CandidateOnly; the preserved baseline supports changed only.' }
if (!$IsWindows) { throw 'This runner requires the Windows private-desktop native helper.' }
. "$PSScriptRoot/benchmark-screenshot.ps1"
. "$PSScriptRoot/benchmark-processes.ps1"
$workspace = (Resolve-Path -LiteralPath "$PSScriptRoot/..").Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Python = (Resolve-Path -LiteralPath $Python).Path
$fixtureScript = (Resolve-Path -LiteralPath "$PSScriptRoot/graph-refresh-fixture.py").Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw "Retaining existing refresh evidence: $OutputDirectory" }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$names = @(@(Get-ChildItem Env: | Where-Object { $_.Name -match '^(GIT_|GIT_RUSTLE_)' } | ForEach-Object Name) + @(
    'LOCALAPPDATA','APPDATA','XDG_CONFIG_HOME','XDG_CACHE_HOME','PYTHONUNBUFFERED','GH_TOKEN','GITHUB_TOKEN','GITLAB_TOKEN',
    'GIT_CONFIG_GLOBAL','GIT_CONFIG_NOSYSTEM','GIT_TERMINAL_PROMPT','GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE',
    'GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_DUMP','GIT_RUSTLE_REFRESH_CONTROL',
    'GIT_RUSTLE_REFRESH_OLD','GIT_RUSTLE_REFRESH_EXPECTED','GIT_RUSTLE_REFRESH_CASE'
) | Select-Object -Unique)
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$results = [Collections.Generic.List[object]]::new()
$startedUtc = [DateTime]::UtcNow.ToString('o')
$failure = $null
$cacheMode = if ($EnableCache) { 'enabled-fresh-isolated' } else { 'disabled' }
$cacheOptionSpecified = $PSBoundParameters.ContainsKey('EnableCache')
function Read-RefreshLog([string]$Path) {
    $stream = $null; $reader = $null
    try {
        $stream = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
        $reader = [IO.StreamReader]::new($stream,[Text.Encoding]::UTF8,$true)
        return $reader.ReadToEnd()
    } catch [IO.IOException] { return '' }
    finally { if ($null -ne $reader) { $reader.Dispose() } elseif ($null -ne $stream) { $stream.Dispose() } }
}
function Refresh-Interval([string]$Text,[string]$End) {
    $first = [regex]::Matches($Text,'(?m)^.*?([0-9.]+) ms\s+refresh native requested\s*$')
    $last = [regex]::Matches($Text,'(?m)^.*?([0-9.]+) ms\s+'+[regex]::Escape($End)+'\s*$')
    # Timing marks stream live and are replayed on exit; require one unique
    # timestamp per mark rather than mistaking the exit replay for another run.
    $starts = @($first | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
    $ends = @($last | ForEach-Object { $_.Groups[1].Value } | Select-Object -Unique)
    if ($starts.Count -ne 1 -or $ends.Count -ne 1) { throw "Expected one unique refresh interval: requested -> $End" }
    $duration = [double]::Parse($ends[0],[Globalization.CultureInfo]::InvariantCulture) - [double]::Parse($starts[0],[Globalization.CultureInfo]::InvariantCulture)
    if ($duration -lt 0) { throw "Negative refresh interval: $End" }
    return $duration
}
function Invoke-FixtureControl([string]$Control,[string]$Endpoint,[string]$Method = 'Get') {
    # Only the freshly created, PID-verified fixture may receive control requests.
    $uri = [Uri]($Control+'/'+$Endpoint)
    if ($uri.Scheme -cne 'http' -or $uri.Host -cne '127.0.0.1' -or $uri.Port -le 0 -or $uri.UserInfo -or $uri.Query -or $uri.Fragment -or $uri.AbsolutePath -cne ('/control/'+$Endpoint)) { throw 'Invalid owned fixture control URL' }
    return Invoke-RestMethod -Uri $uri -Method $Method -TimeoutSec 5 -NoProxy
}
function Save-RefreshStats([string]$Control,[string]$Directory) {
    $stats = Invoke-FixtureControl $Control 'stats'
    $stats | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $Directory 'stats.json')
    return $stats
}
try {
    foreach ($name in $names) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
    $env:GIT_CONFIG_GLOBAL = 'NUL'; $env:GIT_CONFIG_NOSYSTEM = '1'; $env:GIT_TERMINAL_PROMPT = '0'
    $env:GH_TOKEN = 'GRAPH_REFRESH_FIXTURE_TOKEN'
    $env:GIT_RUSTLE_TIMING = '1'; $env:GIT_RUSTLE_TRACE = '1'; $env:PYTHONUNBUFFERED = '1'
    $indices = if ($Capture) { @(0) } else { @(-1..($Samples-1)) }
    foreach ($sample in $indices) {
        # Alternate order within each pair, with one fresh warm-up per executable.
        $order = if ($CandidateOnly) { @('candidate') } elseif (($sample + 1) % 2 -eq 0) { @('baseline','candidate') } else { @('candidate','baseline') }
        foreach ($label in $order) {
            Assert-BenchmarkIdle -WorkspaceRoot $workspace -AuditPath (Join-Path $OutputDirectory 'background-processes.json')
            $suffix = if ($Capture) { 'capture' } elseif ($sample -lt 0) { 'warmup' } else { 'sample-'+$sample.ToString('00') }
            $directory = Join-Path $OutputDirectory "$label-$suffix"
            New-Item -ItemType Directory -Path $directory | Out-Null
            $ownedFixture = Join-Path $directory 'fixture' # Python requires this directory not to exist.
            $env:LOCALAPPDATA = Join-Path $directory 'app-data'; $env:APPDATA = $env:LOCALAPPDATA
            $env:XDG_CONFIG_HOME = Join-Path $directory 'config'; $env:XDG_CACHE_HOME = Join-Path $directory 'cache'
            foreach ($path in @($env:LOCALAPPDATA,$env:XDG_CONFIG_HOME,$env:XDG_CACHE_HOME)) { New-Item -ItemType Directory -Path $path | Out-Null }
            $server = $null; $native = $null; $control = $null; $statsSaved = $false
            $cleanup = [ordered]@{fixtureStopRequested=$false;fixtureKilled=$false;nativeDisposed=$false}
            try {
                $fixtureArguments = @('-u',('"'+$fixtureScript+'"'),'--directory',('"'+$ownedFixture+'"'),'--branches',$Branches,'--tags',$Tags,'--delay-ms',$DelayMs,'--server-only',$ServerOnlyBranches)
                if ($RefreshCase -eq 'unchanged') { $fixtureArguments += '--unchanged' }
                if ($UnrelatedFirst) { $fixtureArguments += '--unrelated-first' }
                if ($ApiOnly) { $fixtureArguments += '--api-only' }
                if ($GitCliOnly) { $fixtureArguments += '--git-cli-only' }
                if ($TrackingOnlyGhost) { $fixtureArguments += '--tracking-only-ghost' }
                if ($MalformedAdvertisement) { $fixtureArguments += '--malformed-advertisement' }
                if ($UnicodeTagAdvertisement) { $fixtureArguments += '--unicode-tag-advertisement' }
                $server = Start-Process -FilePath $Python -WindowStyle Hidden -PassThru `
                    -ArgumentList $fixtureArguments `
                    -RedirectStandardOutput (Join-Path $directory 'fixture.stdout.log') -RedirectStandardError (Join-Path $directory 'fixture.stderr.log')
                $manifestPath = Join-Path $ownedFixture 'manifest.json'
                $deadline = [DateTime]::UtcNow.AddSeconds(30)
                while (!(Test-Path -LiteralPath $manifestPath)) {
                    if ($server.HasExited) { throw "Owned refresh fixture exited before manifest: $directory" }
                    if ([DateTime]::UtcNow -ge $deadline) { throw "Owned refresh fixture manifest timeout: $directory" }
                    Start-Sleep -Milliseconds 30
                }
                # Publication occurs as the last fixture-setup step; retry a partial file read.
                $manifest = $null
                while ($null -eq $manifest) {
                    if ($server.HasExited) { throw "Owned fixture exited while publishing manifest: $directory" }
                    if ([DateTime]::UtcNow -ge $deadline) { throw "Owned fixture manifest publication timeout: $directory" }
                    try { $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json } catch { $manifest = $null }
                    if ($null -eq $manifest) { Start-Sleep -Milliseconds 10 }
                }
                if ([int]$manifest.pid -ne $server.Id) { throw 'Fixture manifest PID does not match the owned hidden process' }
                $expectedRepository = [IO.Path]::GetFullPath((Join-Path $ownedFixture 'repository'))
                if ([IO.Path]::GetFullPath([string]$manifest.repository) -ine $expectedRepository) { throw 'Fixture repository escaped its owned run directory' }
                $control = [string]$manifest.controlUrl
                $initialStats = Invoke-FixtureControl $control 'stats'
                if (!$initialStats.refsUnchanged -or !$initialStats.fetchHeadAbsent -or $initialStats.advanced) { throw 'Fixture initial repository/control state is invalid' }
                $initialStats | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath (Join-Path $directory 'stats-before.json')
                $env:GIT_RUSTLE_SCENARIO = 'refresh-native'
                $env:GIT_RUSTLE_REFRESH_CASE = $RefreshCase
                $env:GIT_RUSTLE_AUTOQUIT = if ($Capture) { '600000' } else { '100' }
                $env:GIT_RUSTLE_DUMP = Join-Path $directory 'state.txt'
                $env:GIT_RUSTLE_REFRESH_CONTROL = $control+'/advance'
                $env:GIT_RUSTLE_REFRESH_OLD = [string]$manifest.oldGhostOid
                $env:GIT_RUSTLE_REFRESH_EXPECTED = [string]$manifest.newGhostOid
                $executable = if ($label -eq 'baseline') { $Baseline } else { $Candidate }
                $log = Join-Path $directory 'app.log'
                $nativeArguments = @(('"'+$manifest.repository+'"'))
                if (!$EnableCache) { $nativeArguments += '--no-cache' }
                $native = Start-BenchmarkNativeProcess -FilePath $executable -ArgumentList $nativeArguments -RedirectStandardError $log
                [pscustomobject]@{label=$label;sample=$sample;capture=[bool]$Capture;cacheMode=$cacheMode;cacheEnabled=[bool]$EnableCache;noCache=(!$EnableCache);cacheOptionSpecified=$cacheOptionSpecified;executable=$executable;nativePid=$native.Id;desktop=$native.DesktopName;fixturePid=$server.Id;fixtureManifest=$manifestPath;controlUrl=$control;startedUtc=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $directory 'launch.json')
                $deadline = [DateTime]::UtcNow.AddSeconds(45)
                $ready = $false
                while (!$native.WaitForExit(0)) {
                    $observed = Read-RefreshLog $log
                    if ($Capture -and $observed -match 'refresh native settled frame' -and $observed -match 'desktop scenario frame') {
                        $ready = $true
                        [pscustomobject]@{observedUtc=[DateTime]::UtcNow.ToString('o');paint='refresh native settled frame + desktop scenario frame';captureExcludedFromTiming=$true} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $directory 'readiness.json')
                        Save-BenchmarkNativeScreenshot -Process $native -OutputPath (Join-Path $directory 'screenshot.png') -CloseAfterCapture -TimeoutSeconds 15 | Out-Null
                        if (!$native.WaitForExit(5000)) { throw "Captured refresh fixture failed to close: $log" }
                        break
                    }
                    if ($server.HasExited) { throw "Fixture exited during native scenario: $directory" }
                    if ([DateTime]::UtcNow -ge $deadline) { throw "Native refresh scenario exceeded 45 seconds: $log" }
                    Start-Sleep -Milliseconds 20
                }
                if ($Capture -and !$ready) { throw "Native scenario exited before its verified screenshot: $log" }
                if ($native.ExitCode -ne 0) { throw "Native refresh scenario failed: $log" }
                $text = Read-RefreshLog $log
                if ($text -notmatch 'desktop scenario frame') { throw "Native refresh scenario did not complete a verified paint: $log" }
                $firstGhost = Refresh-Interval $text 'refresh native first ghost frame'
                $settled = Refresh-Interval $text 'refresh native settled frame'
                if ($settled -lt $firstGhost) { throw 'Settled refresh preceded its first ghost frame' }
                $stats = Save-RefreshStats $control $directory; $statsSaved = $true
                if (!$stats.refsUnchanged -or !$stats.fetchHeadAbsent -or !$stats.filesUnchanged) { throw "Refresh mutated refs or FETCH_HEAD: $directory" }
                if (!$stats.advanced) { throw 'Native scenario did not advance the controlled fixture' }
                $advanced = @($stats.requests | Where-Object { $_.advanced })
                if ($label -eq 'candidate' -and ($UnicodeTagAdvertisement -or $MalformedAdvertisement) -and !$ApiOnly -and !$GitCliOnly -and $Branches -ge 4) {
                    $startupAdvertisements = @($stats.requests | Where-Object { !$_.advanced -and ([string]$_.path).Split('?')[0] -ceq '/team/repo/info/refs' })
                    $nativeStartup = @($startupAdvertisements | Where-Object { $_.nativeHttp }).Count
                    $expectedStartup = if ($MalformedAdvertisement) { 2 } else { 1 }
                    if ($nativeStartup -ne 1 -or $startupAdvertisements.Count -ne $expectedStartup) { throw "Startup advertisement acceptance/fallback did not match fixture: $directory" }
                }
                $startupTags = @($stats.requests | Where-Object { !$_.advanced -and ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo/tags' }).Count
                if ($label -eq 'candidate' -and $RefreshCase -in @('changed','unchanged') -and $startupTags -ne 0) { throw "Graph startup requested remote tag pages: $directory" }
                $projectRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo' }).Count
                $pullRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo/pulls' }).Count
                $pipelineRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo/actions/runs' }).Count
                $full = ($label -eq 'baseline' -and !$BaselineSmart) -or $RefreshCase -eq 'full'
                if ($full -and ($projectRefresh -lt 1 -or $pullRefresh -lt 1)) { throw "Full refresh must exercise provider metadata: $directory" }
                if (!$full -and ($projectRefresh -ne 0 -or $pullRefresh -ne 0)) { throw "Smart refresh requested broad provider metadata: $directory" }
                $tagRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo/tags' }).Count
                $listRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/repos/team/repo/branches' }).Count
                $advertisementRefresh = @($advanced | Where-Object { ([string]$_.path).Split('?')[0] -ceq '/team/repo/info/refs' }).Count
                $httpAdvertisementRefresh = @($advanced | Where-Object { $_.nativeHttp -and ([string]$_.path).Split('?')[0] -ceq '/team/repo/info/refs' }).Count
                if (!$full -and $label -eq 'candidate' -and $Branches -ge 4 -and !$ApiOnly -and !$GitCliOnly -and !$MalformedAdvertisement -and ($httpAdvertisementRefresh -ne 1 -or $advertisementRefresh -ne 1)) { throw "Graph did not use one native HTTP advertisement: $directory" }
                if (!$full -and ($tagRefresh -ne 0 -or ($Branches -lt 64 -and $listRefresh -ne 0))) { throw "Smart refresh requested tags or an unnecessary branch list: $directory" }
                if (!$full -and $label -eq 'candidate' -and $Branches -ge 64) {
                    $individualRefresh = @($advanced | Where-Object { ([string]$_.path).StartsWith('/repos/team/repo/branches/') }).Count
                    if (!$ApiOnly -and ($advertisementRefresh -ne 1 -or $listRefresh -ne 0 -or $individualRefresh -ne 0)) { throw "Large graph did not use one Git ref advertisement: $directory" }
                    if ($ApiOnly -and $advertisementRefresh -ne 0) { throw "Refresh repeated a failed Git transport probe during its cooldown: $directory" }
                    if ($ApiOnly -and ($listRefresh -lt 1 -or (!$UnrelatedFirst -and $individualRefresh -ne 0))) { throw "API-only graph did not fall back to batched verification: $directory" }
                    if ($ApiOnly -and $UnrelatedFirst -and $Branches -eq 64 -and $ServerOnlyBranches -ge 1000 -and ($listRefresh -ne 4 -or $individualRefresh -ne 64)) { throw "Unrelated branch tail did not use the bounded fallback: $directory" }
                    if ($listRefresh -gt [Math]::Ceiling(($Branches+$ServerOnlyBranches)/100)+4) { throw "Large graph repeated its listing: $directory" }
                }
                if ($label -eq 'candidate' -and $RefreshCase -eq 'full' -and ($tagRefresh -lt 1 -or $listRefresh -lt 1)) { throw "Force refresh omitted tags or complete branch list" }
                $localFrame = $null; $tipsFrame = $null
                if ($label -eq 'candidate' -or $BaselineSmart) {
                    $localFrame = Refresh-Interval $text 'refresh native local frame'
                    $tipsFrame = Refresh-Interval $text 'refresh native tips frame'
                }

                if ($label -eq 'candidate' -and ([int]$stats.pipelineRequests -ne 0 -or $pipelineRefresh -ne 0)) { throw "Candidate executed a removed pipeline provider request: $directory" }
                if ($label -eq 'candidate' -and @($stats.forbidden).Count -ne 0) { throw "Candidate requested forbidden pipeline endpoints: $directory" }
                $cacheRoot = Join-Path $env:LOCALAPPDATA 'git-rustle/cache'
                $cacheFiles = @(if (Test-Path -LiteralPath $cacheRoot) { Get-ChildItem -LiteralPath $cacheRoot -File -Recurse | Sort-Object FullName | ForEach-Object { [pscustomobject]@{path=[IO.Path]::GetRelativePath($cacheRoot,$_.FullName);bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName).Hash} } })
                $remoteCacheCount = @($cacheFiles | Where-Object { $_.path -match '^remotes[\\/].+\.heads\.json$' }).Count
                [pscustomobject]@{mode=$cacheMode;enabled=[bool]$EnableCache;root=$cacheRoot;freshDirectoryAtLaunch=$true;remoteCacheFiles=$remoteCacheCount;files=$cacheFiles} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $directory 'cache-evidence.json')
                if ($EnableCache -and $remoteCacheCount -lt 1) { throw "Cache-enabled refresh did not persist an isolated remote-head cache: $directory" }
                $results.Add([pscustomobject]@{label=$label;sample=$sample;warmup=($sample-lt0);capture=[bool]$Capture;cacheMode=$cacheMode;cacheEnabled=[bool]$EnableCache;remoteCacheFiles=$remoteCacheCount;performanceSample=(!$Capture-and$sample-ge0);refreshCase=$RefreshCase;localFrameMs=$localFrame;tipsFrameMs=$tipsFrame;refreshTagRequests=$tagRefresh;refreshListRequests=$listRefresh;refreshAdvertisementRequests=$advertisementRefresh;refreshHttpAdvertisementRequests=$httpAdvertisementRefresh;firstGhostMs=$(if($Capture){$null}else{$firstGhost});settledMs=$(if($Capture){$null}else{$settled});peakRequests=$stats.peakRequests;requestCount=@($stats.requests).Count;forbiddenCount=@($stats.forbidden).Count;projectRequests=[int]$stats.projectRequests;pullRequests=[int]$stats.pullRequests;pipelineRequests=[int]$stats.pipelineRequests;refreshProjectRequests=$projectRefresh;refreshPullRequests=$pullRefresh;refreshPipelineRequests=$pipelineRefresh;startupGraphMs=([double]([regex]::Match($text,'([0-9.]+) ms\s+first frame').Groups[1].Value));startupReadyMs=([double]([regex]::Match($text,'([0-9.]+) ms\s+refresh native requested').Groups[1].Value));providerFairnessVerified=$true;refsUnchanged=[bool]$stats.refsUnchanged;fetchHeadAbsent=[bool]$stats.fetchHeadAbsent;directory=$directory})
                $results.ToArray() | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'samples.json')
            } finally {
                try {
                    if ($null -ne $native) { $native.Dispose(); $cleanup.nativeDisposed = $true }
                } finally {
                  try {
                   if ($null -ne $server) {
                    if ($control -and !$server.HasExited) {
                        if (!$statsSaved) { try { Save-RefreshStats $control $directory | Out-Null } catch { $_.ToString() | Set-Content -LiteralPath (Join-Path $directory 'stats-failure.txt') } }
                        try { Invoke-FixtureControl $control 'stop' 'Post' | Out-Null; $cleanup.fixtureStopRequested = $true } catch { $_.ToString() | Set-Content -LiteralPath (Join-Path $directory 'stop-failure.txt') }
                    }
                    if (!$server.WaitForExit(5000)) { $server.Kill($true); $server.WaitForExit(5000) | Out-Null; $cleanup.fixtureKilled = $true }
                    $server.Dispose()
                   }
                  } finally {
                    $cleanup | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $directory 'cleanup.json')
                  }
                }
            }
        }
    }
    if (!$Capture -and $Samples -ge 15 -and $RefreshCase -ne 'full' -and $Branches -eq 12 -and $DelayMs -eq 60) {
        $durations = @($results | Where-Object { $_.label -eq 'candidate' -and $_.performanceSample } | ForEach-Object { [double]$_.settledMs } | Sort-Object)
        if ($durations.Count -ne $Samples) { throw 'Missing candidate samples for the refresh performance gate' }
        $median = $durations[[int][Math]::Floor($durations.Count/2)]
        $p95 = $durations[[int][Math]::Ceiling($durations.Count*0.95)-1]
        if ($median -gt 1000 -or $p95 -gt 2000) { throw "Smart refresh exceeded the performance gate: median=$median ms; p95=$p95 ms" }
    }
} catch { $failure = $_.ToString(); throw }
finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
    $report = [Collections.Generic.List[string]]::new()
    $report.Add('# Native graph refresh')
    $report.Add('')
    $report.Add('Fresh fixture, isolated app data and hidden localhost server for every launch. Executable order alternates. Raw evidence is retained beside this report.')
    $report.Add('')
    $report.Add('Case: '+$RefreshCase+'. Cache mode: '+$cacheMode+'. Enabled mode starts with an empty isolated application cache for each launch, then measures Refresh after initial load has populated the cache. This does not measure warmed application reopening. cache-evidence.json retains persisted cache hashes. New cases use -CandidateOnly; changed compares the supplied baseline; -BaselineSmart selects the v0.1.15 Smart baseline.')
    $report.Add('')
    if ($Capture) {
        $report.Add('Capture-only run: screenshot durations and trace intervals are excluded from performance samples. Review the actual PNGs separately.')
    } else {
        $report.Add('| Executable | Samples | First ghost median (ms) | First ghost p95 (ms) | Settled median (ms) | Settled p95 (ms) |')
        $report.Add('| --- | ---: | ---: | ---: | ---: | ---: |')
        foreach ($label in @('baseline','candidate')) {
            $rows = @($results | Where-Object { $_.label -eq $label -and $_.performanceSample })
            if ($rows.Count) {
                $metrics = @()
                foreach ($field in @('firstGhostMs','settledMs')) {
                    $values = @($rows | ForEach-Object { [double]$_.$field } | Sort-Object)
                    $middle = [int][Math]::Floor($values.Count/2)
                    $median = if ($values.Count % 2) { $values[$middle] } else { ($values[$middle-1]+$values[$middle])/2 }
                    $p95 = $values[[Math]::Max(0,[int][Math]::Ceiling($values.Count*0.95)-1)]
                    $metrics += $median.ToString('F3',[Globalization.CultureInfo]::InvariantCulture)
                    $metrics += $p95.ToString('F3',[Globalization.CultureInfo]::InvariantCulture)
                }
                $report.Add('| '+$label+' | '+$rows.Count+' | '+($metrics -join ' | ')+' |')
            }
        }
        if ($Samples -lt 15) { $report.Add(''); $report.Add('Smoke run only; fewer than 15 measured samples per executable. Do not treat this as a performance conclusion.') }
    }
    $report.Add('')
    $report.Add('Smart refresh requests no broad provider metadata or tags. Graph scopes of four or more heads use native HTTP ref discovery. Large scopes fall back to Git advertisements then bounded API branch pages. Results are filtered to requested identities. Full Refresh also requests tags and provider metadata. Git refs, objects, index, worktree and stashes remain unchanged. Raw counts and paint intervals are retained. Private-desktop paints exclude physical input, interactive composition, scanout and live-network latency.')
    if ($failure) { $report.Add(''); $report.Add('Run failed: '+$failure) }
    $report.ToArray() | Set-Content -LiteralPath (Join-Path $OutputDirectory 'report.md')
    [pscustomobject]@{startedUtc=$startedUtc;finishedUtc=[DateTime]::UtcNow.ToString('o');success=($null-eq$failure);baselineSmart=[bool]$BaselineSmart;failure=$failure;baseline=$Baseline;baselineSha256=(Get-FileHash -LiteralPath $Baseline).Hash;refreshCase=$RefreshCase;candidateOnly=[bool]$CandidateOnly;serverOnlyBranches=$ServerOnlyBranches;providerFairness='Original v0.1.14 baseline refreshes metadata. Smart candidate does not; Full candidate must refresh metadata, tags and complete branches. Pipelines remain forbidden.';candidate=$Candidate;candidateSha256=(Get-FileHash -LiteralPath $Candidate).Hash;python=$Python;fixtureScript=$fixtureScript;fixtureSha256=(Get-FileHash -LiteralPath $fixtureScript).Hash;runnerSha256=(Get-FileHash -LiteralPath $PSCommandPath).Hash;nativeHelperSha256=(Get-FileHash -LiteralPath "$PSScriptRoot/benchmark-native-process.ps1").Hash;samplesPerExecutable=$(if($Capture){0}else{$Samples});warmupsPerExecutable=$(if($Capture){0}else{1});capture=[bool]$Capture;cacheMode=$cacheMode;cacheEnabled=[bool]$EnableCache;cachePolicy='Fresh isolated disk cache for each launch; enable via -EnableCache; default retains --no-cache. Initial load followed by forced Refresh in the same process.';branches=$Branches;tags=$Tags;unrelatedFirst=[bool]$UnrelatedFirst;apiOnly=[bool]$ApiOnly;gitCliOnly=[bool]$GitCliOnly;trackingOnlyGhost=[bool]$TrackingOnlyGhost;malformedAdvertisement=[bool]$MalformedAdvertisement;unicodeTagAdvertisement=[bool]$UnicodeTagAdvertisement;delayMs=$DelayMs;limitations='Fresh loopback synthetic provider, fresh repository and isolated app configuration for every launch. Alternating executable order; first new ghost and settled refresh end at native GPUI paint marks. Baseline must include the same refresh-native harness backport. Capture runs are separate and their durations are not performance samples. Private-desktop paint/capture excludes physical input, interactive desktop composition, display scanout and real network latency. Existing verified user application may remain running under the shared idle policy.'} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'provenance.json')
}
