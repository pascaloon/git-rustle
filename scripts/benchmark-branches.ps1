param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\branches"
)
# Native browser entry comparison plus candidate-only activity-worker delivery.
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Baseline = (Resolve-Path -LiteralPath $Baseline).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process')
}
function Run-One([string]$exe,[string]$label,[int]$sample) {
    $log = Join-Path $OutputDirectory "$label-$sample.txt"
    $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$label-$sample-screen.txt"
    $process = Start-Process -FilePath $exe -ArgumentList @('"' + $Repository + '"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(30000)) { Stop-Process -Id $process.Id; throw "Timed out: $log" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $frame = [regex]::Match($text,'([\d.]+) ms  branch browser frame')
    $requested = [regex]::Match($text,'([\d.]+) ms  branch activity requested')
    $ready = [regex]::Match($text,'([\d.]+) ms  branch activity ready')
    if (-not $frame.Success) { throw "Missing branch browser frame: $log" }
    if ($label -eq 'candidate' -and (-not $ready.Success -or -not $requested.Success)) { throw "Missing activity delivery: $log" }
    $parse = { param($match) [double]::Parse($match.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    [pscustomobject]@{
        binary=$label; sample=$sample; browserFrameMs= & $parse $frame
        activityWorkerMs=if ($ready.Success) { (& $parse $ready) - (& $parse $requested) } else { $null }
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $env:GIT_RUSTLE_AUTOQUIT='500'
    $env:GIT_RUSTLE_KEYS='b'
    Run-One $Baseline 'baseline' -1 | Out-Null
    Run-One $Candidate 'candidate' -1 | Out-Null
    $results=@()
    for ($i=0;$i -lt $Samples;$i++) {
        if ($i % 2 -eq 0) { $results+=Run-One $Baseline 'baseline' $i; $results+=Run-One $Candidate 'candidate' $i }
        else { $results+=Run-One $Candidate 'candidate' $i; $results+=Run-One $Baseline 'baseline' $i }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $results | Format-Table -AutoSize
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
