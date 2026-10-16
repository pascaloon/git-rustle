$ErrorActionPreference='Stop'
$headRun=Get-Content target/bench/head-label-run.txt
$fixture=Join-Path $headRun 'native-fixture'
$candidate=(Resolve-Path target/release/git-rustle.exe).Path
$baseline=Join-Path $headRun 'baseline.exe'
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP','GIT_RUSTLE_CHECKOUT_FIXTURE')) {
    $saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')
}
function Check-Screen([string]$exe,[string]$label,[bool]$expectHead) {
    $env:GIT_RUSTLE_DUMP=Join-Path $headRun "$label-screen.txt"
    $log=Join-Path $headRun "$label-native.txt"
    $process=Start-Process -FilePath $exe -ArgumentList @('"'+$fixture+'"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id; throw "Native screen timeout: $label" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Native screen failed: $label" }
    $headId=(git -C $fixture rev-parse --short=7 HEAD).Trim()
    $screen=Get-Content $env:GIT_RUSTLE_DUMP -Raw
    $row=[regex]::Match($screen,'(?m)^.*\b'+$headId+' +(?<labels>[^\r\n]*)')
    if (-not $row.Success) { throw "HEAD commit not visible: $label" }
    $count=[regex]::Matches($row.Groups['labels'].Value,'\bHEAD\b').Count
    if (($expectHead -and $count -ne 1) -or (-not $expectHead -and $count -ne 0)) { throw "Unexpected HEAD badge count $count for $label" }
    [pscustomobject]@{mode=$label;head=$headId;headBadges=$count;row=$row.Value.Trim()}
}
try {
    $env:GIT_RUSTLE_CHECKOUT_FIXTURE=$fixture
    cargo test --release checkout::tests::prepare_native_fixture -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Fixture preparation failed' }
    $env:GIT_RUSTLE_TIMING='1'
    $env:GIT_RUSTLE_AUTOQUIT='250'
    $env:GIT_RUSTLE_KEYS='H'
    $checks=@()
    $checks+=Check-Screen $baseline 'baseline-attached' $false
    $checks+=Check-Screen $candidate 'candidate-attached' $true
    if (-not (Test-Path -LiteralPath (Join-Path $fixture '.git/rustle-checkout-benchmark'))) { throw 'Missing disposable fixture marker' }
    git -C $fixture switch --detach 'HEAD~1'
    if ($LASTEXITCODE -ne 0) { throw 'Fixture detach failed' }
    $checks+=Check-Screen $candidate 'candidate-detached' $true
    git -C $fixture switch main
    if ($LASTEXITCODE -ne 0) { throw 'Fixture restore failed' }
    $checks | ConvertTo-Json | Set-Content (Join-Path $headRun 'native-checks.json')
    $checks | Format-Table -AutoSize
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
