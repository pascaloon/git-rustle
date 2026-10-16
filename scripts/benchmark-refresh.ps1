param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\refresh"
)
# Compare the former Graph r binding against F5 without changing repository refs.
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
    # Ten unbound keys allow initial discovery to complete before manual refresh.
    $env:GIT_RUSTLE_KEYS = if ($label -eq 'baseline') { 'xxxxxxxxxxr' } else { 'xxxxxxxxxx<f5>' }
    $process = Start-Process -FilePath $exe -ArgumentList @('"' + $Repository + '"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(30000)) { Stop-Process -Id $process.Id; throw "Timed out: $log" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $requested = [regex]::Matches($text,'([\d.]+) ms  remote discovery requested')
    $ready = [regex]::Matches($text,'([\d.]+) ms  remote discovery done')
    if ($requested.Count -ne 2 -or $ready.Count -ne 2) { throw "Expected startup discovery and manual refresh: $log" }
    $parse = { param($match) [double]::Parse($match.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    if ((& $parse $requested[1]) -lt (& $parse $ready[0])) { throw "Refresh overlapped startup discovery: $log" }
    if ($label -eq 'candidate' -and (Get-Content $env:GIT_RUSTLE_DUMP -Raw) -notmatch 'F5') { throw "Missing F5 screen hint: $log" }
    [pscustomobject]@{
        binary=$label; sample=$sample
        refreshDiscoveryMs=(& $parse $ready[1]) - (& $parse $requested[1])
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $env:GIT_RUSTLE_AUTOQUIT='2000'
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
