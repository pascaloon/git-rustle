param(
    [Parameter(Mandatory)][string]$Repository,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [string[]]$Scenarios = @('scroll','branches','diff','search','settings'),
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\desktop"
)
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved = @{}
$names = @('GIT_RUSTLE_TIMING','GIT_RUSTLE_TRACE','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_DUMP','GIT_RUSTLE_SCENARIO','GIT_RUSTLE_FRAME_SAMPLES')
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
function Run-One([string]$scenario,[int]$sample) {
    $log = Join-Path $OutputDirectory "$scenario-$sample.txt"
    $env:GIT_RUSTLE_SCENARIO = $scenario
    $env:GIT_RUSTLE_FRAME_SAMPLES = Join-Path $OutputDirectory "$scenario-$sample-frames.json"
    $env:GIT_RUSTLE_DUMP = Join-Path $OutputDirectory "$scenario-$sample-state.txt"
    $p = Start-Process -FilePath $Candidate -ArgumentList @('"' + $Repository + '"') -WindowStyle Normal -RedirectStandardError $log -PassThru
    if (-not $p.WaitForExit(60000)) { Stop-Process -Id $p.Id; throw "Timeout: $log" }
    $p.Refresh()
    if ($p.ExitCode -ne 0) { throw "Failed: $log" }
    $text = Get-Content -LiteralPath $log -Raw
    $marks = @{}
    foreach ($m in [regex]::Matches($text,'([\d.]+) ms  ([^\r\n]+)')) {
        if (-not $marks.ContainsKey($m.Groups[2].Value)) { $marks[$m.Groups[2].Value] = [double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture) }
    }
    if (-not $marks.ContainsKey('desktop scenario frame')) { throw "Missing rendered scenario frame: $log" }
    $metric = switch ($scenario) {
        'scroll' { 'desktop scroll done' }
        'files-scroll' { 'desktop scroll done' }
        'diff-scroll' { 'desktop scroll done' }
        'diff-pan' { 'desktop scroll done' }
        'search-scroll' { 'desktop scroll done' }
        'compare' { 'file diff (' }
        'branches' { 'branch activity ready' }
        'diff' { 'file diff (' }
        'search' { 'search ready' }
        'settings' { 'settings completed' }
        'pending' { 'pending status ready' }
        'pending-diff' { 'file diff (' }
    }
    $end = @($marks.Keys | Where-Object { $_.StartsWith($metric) } | Sort-Object)[0]
    if (-not $end) { throw "Missing $metric in $log" }
    $start = switch ($scenario) {
        'branches' { 'branch activity requested' }
        'diff' { 'file diff requested' }
        'search' {
            if ($end -notmatch '^search ready \((\d+);') { throw "Unrecognized search result: $end" }
            "search requested ($($Matches[1]))"
        }
        'settings' { 'settings requested' }
        'pending' { 'pending status requested' }
        'pending-diff' { 'file diff requested' }
        'compare' { 'file diff requested' }
    }
    $scrolling = $scenario -in @('scroll','files-scroll','diff-scroll','diff-pan','search-scroll')
    $frames = if ($scrolling) { @(Get-Content $env:GIT_RUSTLE_FRAME_SAMPLES -Raw | ConvertFrom-Json) } else { @() }
    if ($scrolling -and $frames.Count -ne 120) { throw 'Expected 120 measured frames' }
    [pscustomobject]@{scenario=$scenario;sample=$sample;readySinceMainMs=$marks[$end];workerMs=if($start){$marks[$end]-$marks[$start]}else{$null};frameIntervalsMs=$frames}
}
try {
    $env:GIT_RUSTLE_TIMING='1'; $env:GIT_RUSTLE_AUTOQUIT='100'
    Remove-Item Env:GIT_RUSTLE_TRACE -ErrorAction SilentlyContinue
    $results=@()
    foreach ($scenario in $Scenarios) {
        Run-One $scenario -1 | Out-Null
        for ($i=0;$i -lt $Samples;$i++) { $results += Run-One $scenario $i }
    }
    $results | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutputDirectory 'samples.json')
} finally { foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') } }
