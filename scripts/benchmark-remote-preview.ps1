param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [switch]$NativeCandidate,
    [switch]$NativeBaseline,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\remote-preview"
)
# Use the fresh fixture from remote::history::tests::prepare_native_fixture.
# Both executables read identical branch/metadata caches; no server is needed.
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING', 'GIT_RUSTLE_AUTOQUIT', 'GIT_RUSTLE_KEYS', 'GIT_RUSTLE_DUMP')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
function Run-One([string]$exe, [string]$label, [int]$sample) {
    $log = Join-Path $OutputDirectory "$label-$sample.txt"
    $windowStyle = if (($NativeCandidate -and $label -eq 'candidate') -or ($NativeBaseline -and $label -eq 'baseline')) { 'Normal' } else { 'Hidden' }
    $process = Start-Process -FilePath $exe -ArgumentList @('"' + $Repository + '"') -WindowStyle $windowStyle -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(30000)) { Stop-Process -Id $process.Id; throw "Timed out: $log" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $start = [regex]::Match($text, '([\d.]+) ms  ghost preview requested')
    $ready = [regex]::Match($text, '([\d.]+) ms  ghost preview ready \(2 commits\)')
    if (-not $start.Success -or -not $ready.Success) { throw "Missing cached preview: $log" }
    $a = [double]::Parse($start.Groups[1].Value, [cultureinfo]::InvariantCulture)
    $b = [double]::Parse($ready.Groups[1].Value, [cultureinfo]::InvariantCulture)
    [pscustomobject]@{ binary = $label; sample = $sample; previewWorkerMs = $b - $a; previewReadyMs = $b }
}
function Percentile($values, [double]$p) {
    $sorted = @($values | Sort-Object)
    $sorted[[Math]::Max(0, [Math]::Ceiling($sorted.Count * $p) - 1)]
}
try {
    $env:GIT_RUSTLE_TIMING = '1'
    $env:GIT_RUSTLE_AUTOQUIT = '250'
    Remove-Item Env:GIT_RUSTLE_KEYS -ErrorAction SilentlyContinue
    Remove-Item Env:GIT_RUSTLE_DUMP -ErrorAction SilentlyContinue
    Run-One $Baseline 'baseline' -1 | Out-Null
    Run-One $Candidate 'candidate' -1 | Out-Null
    $results = @()
    for ($i = 0; $i -lt $Samples; $i++) {
        if ($i % 2 -eq 0) {
            $results += Run-One $Baseline 'baseline' $i
            $results += Run-One $Candidate 'candidate' $i
        } else {
            $results += Run-One $Candidate 'candidate' $i
            $results += Run-One $Baseline 'baseline' $i
        }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $summary = foreach ($group in ($results | Group-Object binary)) {
        [pscustomobject]@{
            binary = $group.Name
            workerMedianMs = Percentile $group.Group.previewWorkerMs 0.5
            workerP95Ms = Percentile $group.Group.previewWorkerMs 0.95
            readyMedianMs = Percentile $group.Group.previewReadyMs 0.5
            readyP95Ms = Percentile $group.Group.previewReadyMs 0.95
        }
    }
    $summary | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'summary.json')
    $summary | Format-Table -AutoSize
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process') }
}
