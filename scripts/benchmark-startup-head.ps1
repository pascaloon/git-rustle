param(
    [Parameter(Mandatory)][string]$Baseline,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [Parameter(Mandatory)][string]$OutputDirectory
)
# Native startup comparison with HEAD below the initial viewport; no scripted input.
$ErrorActionPreference='Stop'
$Baseline=(Resolve-Path -LiteralPath $Baseline).Path
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
$fixture=Join-Path $OutputDirectory 'fixture'
if (Test-Path -LiteralPath $fixture) { throw 'Use a new output directory for the disposable fixture' }
New-Item -ItemType Directory -Path $fixture | Out-Null
git -C $fixture init -q -b main
if ($LASTEXITCODE -ne 0) { throw 'Fixture init failed' }
git -C $fixture config core.hooksPath disabled-benchmark-hooks
$stream=New-Object System.Text.StringBuilder
for ($n=1;$n -le 120;$n++) {
    $date=1700000000+$n
    $message='Startup commit {0:000}' -f $n
    [void]$stream.Append("commit refs/heads/main`nmark :$n`nauthor Startup Test <test@example.invalid> $date +0000`ncommitter Startup Test <test@example.invalid> $date +0000`ndata $($message.Length)`n$message`n")
    if ($n -gt 1) { [void]$stream.Append("from :$($n-1)`n") }
    $content="version $n`n"
    [void]$stream.Append("M 100644 inline file.txt`ndata $($content.Length)`n$content`n")
}
$importPath=Join-Path $OutputDirectory 'fixture-import.txt'
[IO.File]::WriteAllText($importPath,$stream.ToString(),[Text.UTF8Encoding]::new($false))
$import=Start-Process -FilePath git -ArgumentList @('-C',('"'+$fixture+'"'),'fast-import','--quiet') -WindowStyle Hidden -RedirectStandardInput $importPath -RedirectStandardError (Join-Path $OutputDirectory 'fixture-import-error.txt') -PassThru
if (-not $import.WaitForExit(10000)) { Stop-Process -Id $import.Id; throw 'Fixture import timeout' }
$import.Refresh()
if ($import.ExitCode -ne 0) { throw 'Fixture import failed' }
git -C $fixture switch -q -c topic 'main~100'
if ($LASTEXITCODE -ne 0) { throw 'Fixture branch failed' }
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {
    $saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')
}
function Run-One([string]$exe,[string]$label,[string]$state,[string]$mode,[int]$sample) {
    $stem="$label-$state-$mode-$sample"
    $log=Join-Path $OutputDirectory "$stem.txt"
    $env:GIT_RUSTLE_DUMP=Join-Path $OutputDirectory "$stem-screen.txt"
    $runArgs=@('"'+$fixture+'"')
    if ($mode -eq 'uncached') { $runArgs+='--no-cache' }
    $process=Start-Process -FilePath $exe -ArgumentList $runArgs -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id; throw "Timeout: $log" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $timing=Get-Content $log -Raw
    $frame=[regex]::Match($timing,'([\d.]+) ms  first frame')
    if (-not $frame.Success) { throw "Missing first frame: $log" }
    $selected=if ($label -eq 'baseline') { '120' } elseif ($state -eq 'attached') { '020' } else { '010' }
    $screen=Get-Content $env:GIT_RUSTLE_DUMP -Raw
    if ($screen -notmatch ('(?m)│ +Startup commit '+$selected+'\s*$')) { throw "Wrong initial selection: $log" }
    if ($label -eq 'candidate' -and $screen -notmatch '\bHEAD +') { throw "HEAD not visible: $log" }
    [pscustomobject]@{binary=$label;state=$state;mode=$mode;sample=$sample;firstFrameMs=[double]::Parse($frame.Groups[1].Value,[cultureinfo]::InvariantCulture);selectedSubject="Startup commit $selected"}
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $env:GIT_RUSTLE_AUTOQUIT='50'
    Remove-Item Env:GIT_RUSTLE_KEYS -ErrorAction SilentlyContinue
    $results=@()
    foreach ($state in @('attached','detached')) {
        if ($state -eq 'detached') {
            git -C $fixture switch -q --detach 'main~110'
            if ($LASTEXITCODE -ne 0) { throw 'Fixture detach failed' }
        }
        foreach ($mode in @('warm','uncached')) {
            Run-One $Baseline 'baseline' $state $mode -1 | Out-Null
            Run-One $Candidate 'candidate' $state $mode -1 | Out-Null
            for ($n=0;$n -lt $Samples;$n++) {
                if ($n % 2 -eq 0) { $results+=Run-One $Baseline 'baseline' $state $mode $n; $results+=Run-One $Candidate 'candidate' $state $mode $n }
                else { $results+=Run-One $Candidate 'candidate' $state $mode $n; $results+=Run-One $Baseline 'baseline' $state $mode $n }
            }
        }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $results | Group-Object binary,state,mode | ForEach-Object {
        $values=@($_.Group.firstFrameMs | Sort-Object)
        [pscustomobject]@{group=$_.Name;medianMs=$values[[Math]::Ceiling($values.Count*0.5)-1];p95Ms=$values[[Math]::Ceiling($values.Count*0.95)-1]}
    } | Format-Table -AutoSize
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
