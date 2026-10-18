param(
    [Parameter(Mandatory)][string]$Candidate,
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [int]$Samples = 15
)
$ErrorActionPreference = 'Stop'
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
$Repository = (Resolve-Path -LiteralPath $Repository).Path
New-Item -ItemType Directory -Path $OutputDirectory -ErrorAction Stop | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
# Output normally lives inside a Git checkout. Use a genuinely unrelated CWD.
$empty = Join-Path $env:TEMP ('git-rustle-landing-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $empty | Out-Null
$empty | Set-Content (Join-Path $OutputDirectory 'fixture-directory.txt')
$history = Join-Path $OutputDirectory 'recent.json'
$names = @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_RECENTS_FILE','GIT_RUSTLE_LANDING_REPO')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
function Mark($text, $label) {
    $match = [regex]::Match($text, "([\d.]+) ms  $label")
    if (-not $match.Success) { throw "Missing timing: $label" }
    [double]::Parse($match.Groups[1].Value, [cultureinfo]::InvariantCulture)
}
try {
    $env:GIT_RUSTLE_TIMING = '1'
    $env:GIT_RUSTLE_AUTOQUIT = '10'
    $env:GIT_RUSTLE_RECENTS_FILE = $history
    $env:GIT_RUSTLE_LANDING_REPO = $Repository
    $results = @()
    foreach ($scenario in @('landing','landing-open','landing-forget')) {
        $env:GIT_RUSTLE_SCENARIO = if ($scenario -eq 'landing') { $null } else { $scenario }
        for ($sample = -1; $sample -lt $Samples; $sample++) {
            if (@(Get-Process cargo,rustc,cl,link -ErrorAction SilentlyContinue).Count) { throw 'Competing build' }
            # Twenty entries, including stale/missing paths: rendering must not probe them.
            $entries = @($Repository) + @(1..19 | ForEach-Object { Join-Path $empty "missing-$_" })
            ConvertTo-Json -InputObject $entries | Set-Content -LiteralPath $history
            $log = Join-Path $OutputDirectory "$scenario-$sample.txt"
            $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$scenario-$sample-state.txt"
            $p = Start-Process -FilePath $Candidate -ArgumentList @('"' + $empty + '"') -WindowStyle Normal -RedirectStandardError $log -PassThru
            if (-not $p.WaitForExit(60000)) { Stop-Process -Id $p.Id; throw "Timeout: $log" }
            $p.Refresh()
            if ($p.ExitCode -ne 0) { throw "Failed: $log" }
            $text = Get-Content -LiteralPath $log -Raw
            $state = Get-Content -LiteralPath $env:GIT_RUSTLE_DUMP -Raw
            $stored = @(Get-Content -LiteralPath $history -Raw | ConvertFrom-Json)
            $landing = Mark $text 'landing frame'
            $recentReady = Mark $text 'recent repositories ready'
            if ($recentReady -gt $landing) { throw "Landing frame did not include loaded recent history: $log" }
            $operation = $null
            if ($scenario -eq 'landing-open') {
                if ($state -match 'landing page' -or $state -notmatch 'viewport_rows=\[[1-9]') { throw "Repository did not open: $log" }
                $frames = [regex]::Matches($text, '([\d.]+) ms  first frame')
                if ($frames.Count -ne 2) { throw "Expected landing and repository frames: $log" }
                $operation = [double]::Parse($frames[1].Groups[1].Value, [cultureinfo]::InvariantCulture)
                if ($stored.Count -ne 20 -or $stored[0] -notlike "*$((Split-Path $Repository -Leaf))") { throw 'MRU update failed' }
            } elseif ($scenario -eq 'landing-forget') {
                if ($state -notmatch 'recent_count=19' -or $stored.Count -ne 19 -or $stored -contains $Repository) { throw 'Forget failed' }
                $operation = (Mark $text 'forget repository frame') - (Mark $text 'forget repository requested')
            } elseif ($state -notmatch 'landing page' -or $state -notmatch 'recent_count=20') { throw 'Landing state invalid' }
            if ($sample -ge 0) {
                $results += [pscustomobject]@{ scenario=$scenario; sample=$sample; landingFrameMs=$landing; recentReadyMs=$recentReady; operationMs=$operation }
            }
        }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $summary = foreach ($group in ($results | Group-Object scenario)) {
        $landing = @($group.Group.landingFrameMs | Sort-Object)
        $operation = @($group.Group.operationMs | Where-Object { $null -ne $_ } | Sort-Object)
        [pscustomobject]@{
            scenario=$group.Name; samples=$group.Count
            landingMedianMs=$landing[[Math]::Ceiling($landing.Count * 0.5)-1]
            landingP95Ms=$landing[[Math]::Ceiling($landing.Count * 0.95)-1]
            operationMedianMs=if ($operation.Count) { $operation[[Math]::Ceiling($operation.Count * 0.5)-1] } else { $null }
            operationP95Ms=if ($operation.Count) { $operation[[Math]::Ceiling($operation.Count * 0.95)-1] } else { $null }
        }
    }
    $summary | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'summary.json')
    $summary | Format-Table -AutoSize
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
