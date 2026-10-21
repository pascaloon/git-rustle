param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [switch]$RejectCompetingBuilds,
    [int]$Samples = 15,
    [switch]$NativeCandidate,
    [switch]$NativeBaseline,
    # Separate binary caches when cache formats differ between revisions.
    [string]$IsolatedCacheRoot,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\comparison"
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/benchmark-processes.ps1"
. "$PSScriptRoot/benchmark-native-process.ps1"
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING', 'GIT_RUSTLE_AUTOQUIT', 'GIT_RUSTLE_KEYS', 'GIT_RUSTLE_DUMP', 'LOCALAPPDATA')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
function Run-One([string]$exe, [string]$label, [string]$mode, [int]$sample) {
    if ($IsolatedCacheRoot) {
        $env:LOCALAPPDATA = Join-Path $IsolatedCacheRoot $label
        New-Item -ItemType Directory -Force -Path $env:LOCALAPPDATA | Out-Null
    }
    if ($RejectCompetingBuilds) { Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
    $log = Join-Path $OutputDirectory "$label-$mode-$sample.txt"
    $runArgs = @('"' + $Repository + '"')
    if ($mode -eq 'uncached') { $runArgs += '--no-cache' }
    $process = Start-BenchmarkNativeProcess -FilePath $exe -ArgumentList $runArgs -RedirectStandardError $log
    try {
        if (-not $process.WaitForExit(120000)) {
            $process.Kill()
            throw "Benchmark exceeded two minutes: $log"
        }
        $process.Refresh()
        $exitCode = $process.ExitCode
    } finally { $process.Dispose() }
    if ($RejectCompetingBuilds) { Assert-BenchmarkIdle -WorkspaceRoot (Resolve-Path "$PSScriptRoot/..").Path -AuditPath (Join-Path $OutputDirectory 'background-processes.json') }
    if ($exitCode -ne 0) { throw "Benchmark failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $frame = [regex]::Match($text, '([\d.]+) ms  first frame')
    $walk = [regex]::Match($text, '([\d.]+) ms  walk done')
    if (-not $frame.Success) { throw "Missing first-frame timing: $log" }
    [pscustomobject]@{
        binary = $label; mode = $mode; sample = $sample
        firstFrameMs = [double]::Parse($frame.Groups[1].Value, [cultureinfo]::InvariantCulture)
        walkDoneMs = if ($walk.Success) { [double]::Parse($walk.Groups[1].Value, [cultureinfo]::InvariantCulture) } else { $null }
    }
}
function Percentile($values, [double]$p) {
    $sorted = @($values | Sort-Object)
    $sorted[[Math]::Max(0, [Math]::Ceiling($sorted.Count * $p) - 1)]
}
try {
    $env:GIT_RUSTLE_TIMING = '1'
    $env:GIT_RUSTLE_AUTOQUIT = '10'
    Remove-Item Env:GIT_RUSTLE_KEYS -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_RUSTLE_DUMP -ErrorAction SilentlyContinue
    $results = @()
    foreach ($mode in @('warm', 'uncached')) {
        # Warm up executable/dependency pages and, in warm mode, the layout cache.
        Run-One $Baseline 'baseline' $mode -1 | Out-Null
        Run-One $Candidate 'candidate' $mode -1 | Out-Null
        for ($i = 0; $i -lt $Samples; $i++) {
            # Alternate order to reduce time/temperature bias.
            if ($i % 2 -eq 0) {
                $results += Run-One $Baseline 'baseline' $mode $i
                $results += Run-One $Candidate 'candidate' $mode $i
            } else {
                $results += Run-One $Candidate 'candidate' $mode $i
                $results += Run-One $Baseline 'baseline' $mode $i
            }
        }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $summary = foreach ($group in ($results | Group-Object binary, mode)) {
        [pscustomobject]@{
            binary = $group.Group[0].binary; mode = $group.Group[0].mode; samples = $group.Count
            frameMedianMs = Percentile $group.Group.firstFrameMs 0.5
            frameP95Ms = Percentile $group.Group.firstFrameMs 0.95
            walkMedianMs = if ($group.Group[0].mode -eq 'uncached') { Percentile $group.Group.walkDoneMs 0.5 } else { $null }
        }
    }
    $summary | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'summary.json')
    $summary | Format-Table -AutoSize
    Get-Item $Baseline, $Candidate | Select-Object FullName, Length
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
