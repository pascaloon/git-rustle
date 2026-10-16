param(
    [Parameter(Mandatory)][string]$Repository,
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\checkout"
)
# Popup-only runs use the large repository; all confirmed checkouts use the
# disposable three-commit fixture from checkout::tests::prepare_native_fixture.
$ErrorActionPreference='Stop'
$Repository=(Resolve-Path -LiteralPath $Repository).Path
$Fixture=(Resolve-Path -LiteralPath $Fixture).Path
$Baseline=(Resolve-Path -LiteralPath $Baseline).Path
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
if ($Repository -eq $Fixture) {throw 'Checkout fixture must be disposable and distinct from the benchmark repository'}
$marker=Join-Path $Fixture '.git/rustle-checkout-benchmark'
if (-not (Test-Path -LiteralPath $marker) -or (Get-Content -LiteralPath $marker -Raw) -ne 'git-rustle checkout benchmark fixture v1') {throw 'Use a fresh fixture prepared by checkout::tests::prepare_native_fixture'}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {
    $saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')
}
function Fixture-Git([string[]]$gitArgs) {
    $value=& git -C $Fixture @gitArgs
    if ($LASTEXITCODE -ne 0) {throw "Fixture Git command failed: $gitArgs"}
    $value
}
$root=Fixture-Git -gitArgs @('rev-parse','main~2')
$topic=Fixture-Git -gitArgs @('rev-parse','topic')
function Run-One([string]$mode,[string]$binary,[int]$sample) {
    $exe=if ($binary -eq 'baseline') {$Baseline} else {$Candidate}
    $popup=$mode -like 'popup-*'
    if (-not $popup) { Fixture-Git -gitArgs @('switch','--quiet','main') | Out-Null }
    $repo=if ($popup) {$Repository} else {$Fixture}
    $script=switch ($mode) {
        'popup-graph' {'<enter>'}
        'popup-browser' {'b<enter>'}
        'graph-branch' {'<home><down><enter><enter>'}
        'graph-commit' {'<end><enter><enter>'}
        'browser-branch' {if ($binary -eq 'baseline') {'btopic<enter>'} else {'btopic<enter><enter>'}}
    }
    $env:GIT_RUSTLE_KEYS=$script
    $env:GIT_RUSTLE_AUTOQUIT=if ($popup) {'400'} else {'900'}
    $log=Join-Path $OutputDirectory "$mode-$binary-$sample.txt"
    $dump=Join-Path $OutputDirectory "$mode-$binary-$sample-screen.txt"
    $env:GIT_RUSTLE_DUMP=$dump
    $process=Start-Process -FilePath $exe -ArgumentList @('"'+$repo+'"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(30000)) {Stop-Process -Id $process.Id; throw "Timeout: $log"}
    $process.Refresh()
    if ($process.ExitCode -ne 0) {throw "Failed: $log"}
    $text=Get-Content -LiteralPath $log -Raw
    $screen=Get-Content -LiteralPath $dump -Raw
    $events=@([regex]::Matches($text,'([\d.]+) ms  (.+)') | ForEach-Object {
        [pscustomobject]@{ms=[double]::Parse($_.Groups[1].Value,[cultureinfo]::InvariantCulture);event=$_.Groups[2].Value}
    })
    if ($popup) {
        $frame=@($events | Where-Object event -eq 'checkout confirmation frame')[0]
        $inputMark=@($events | Where-Object { $_.event -eq 'enter input' -and $_.ms -le $frame.ms })[-1]
        if (-not $frame -or -not $inputMark -or $screen -notmatch 'Checkout (branch|commit)\?' -or $text -match 'checkout requested') {throw "Missing read-only confirmation: $log"}
        [pscustomobject]@{mode=$mode;binary=$binary;sample=$sample;popupFrameMs=$frame.ms-$inputMark.ms;checkoutWorkerMs=$null}
    } else {
        $start=@($events | Where-Object event -eq 'checkout requested')[-1]
        $done=@($events | Where-Object event -eq 'checkout done')[-1]
        if (-not $start -or -not $done -or $screen -match 'Checkout failed:') {throw "Missing successful checkout: $log"}
        $head=Fixture-Git -gitArgs @('rev-parse','HEAD')
        $branch=Fixture-Git -gitArgs @('branch','--show-current')
        if ($mode -eq 'graph-commit') {
            if ($head -ne $root -or $branch) {throw "Expected detached root: $log"}
        } elseif ($head -ne $topic -or $branch -ne 'topic') {throw "Expected topic branch: $log"}
        [pscustomobject]@{mode=$mode;binary=$binary;sample=$sample;popupFrameMs=$null;checkoutWorkerMs=$done.ms-$start.ms}
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $results=@()
    foreach ($mode in @('popup-graph','popup-browser','graph-branch','graph-commit')) {
        Run-One $mode 'candidate' -1 | Out-Null
        for ($i=0;$i -lt $Samples;$i++) {$results+=Run-One $mode 'candidate' $i}
    }
    Run-One 'browser-branch' 'baseline' -1 | Out-Null
    Run-One 'browser-branch' 'candidate' -1 | Out-Null
    for ($i=0;$i -lt $Samples;$i++) {
        if ($i % 2 -eq 0) {$results+=Run-One 'browser-branch' 'baseline' $i; $results+=Run-One 'browser-branch' 'candidate' $i}
        else {$results+=Run-One 'browser-branch' 'candidate' $i; $results+=Run-One 'browser-branch' 'baseline' $i}
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $results | Format-Table -AutoSize
} finally {
    Fixture-Git -gitArgs @('switch','--quiet','main') | Out-Null
    foreach ($name in $saved.Keys) {[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
}
