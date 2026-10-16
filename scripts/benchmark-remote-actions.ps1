param(
    [Parameter(Mandatory)][string]$Repository,
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples = 15,
    [string]$OutputDirectory = "$PSScriptRoot\..\target\bench\remote-actions"
)
# Candidate-only measurements: the baseline cannot preview remote-only branches
# on Tab or mark remote pairs. Use a fresh prepare_native_fixture directory.
$ErrorActionPreference = 'Stop'
$Repository = (Resolve-Path -LiteralPath $Repository).Path
$Candidate = (Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {
    $saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')
}
function Run-One([string]$mode,[int]$sample) {
    $log=Join-Path $OutputDirectory "$mode-$sample.txt"
    $dump=Join-Path $OutputDirectory "$mode-$sample-screen.txt"
    $env:GIT_RUSTLE_DUMP=$dump
    $env:GIT_RUSTLE_KEYS=if ($mode -eq 'tab') { 'bnot-local<tab>' } else { 'bnot-local<tab>xxxxxx<space>b<c-u>origin/main<tab>xxxx<space>d' }
    $env:GIT_RUSTLE_AUTOQUIT=if ($mode -eq 'tab') { '1000' } else { '2200' }
    $process=Start-Process -FilePath $Candidate -ArgumentList @('"' + $Repository + '"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(30000)) { Stop-Process -Id $process.Id; throw "Timed out: $log" }
    $process.Refresh()
    if ($process.ExitCode -ne 0) { throw "Failed: $log" }
    $text=Get-Content -LiteralPath $log -Raw
    $screen=Get-Content -LiteralPath $dump -Raw
    $events=@([regex]::Matches($text,'([\d.]+) ms  (.+)') | ForEach-Object {
        [pscustomobject]@{ ms=[double]::Parse($_.Groups[1].Value,[cultureinfo]::InvariantCulture); event=$_.Groups[2].Value }
    })
    $preview=@($events | Where-Object event -eq 'ghost preview ready (3 commits)')[-1]
    if (-not $preview) { throw "Missing remote-only preview: $log" }
    $requested=@($events | Where-Object { $_.event -eq 'ghost preview requested' -and $_.ms -le $preview.ms })[-1]
    if ($mode -eq 'tab') {
        if ($screen -notmatch 'origin/not-local' -or $screen -match 'Branches \u00b7') { throw "Tab did not navigate: $dump" }
        [pscustomobject]@{ mode=$mode; sample=$sample; tabWorkerMs=$preview.ms-$requested.ms; pairTreeMs=$null; pairReadyMs=$null }
    } else {
        if ($screen -notmatch 'fffffff \u2192 aaaaaaa' -or $screen -notmatch 'Remote-only' -or $screen -notmatch 'Remote file') { throw "Missing endpoint pair diff: $dump" }
        $file=@($events | Where-Object { $_.event -like 'file diff (*' })[-1]
        $tree=@($events | Where-Object { $_.event -eq 'tree diff (1 files)' -and $_.ms -le $file.ms })[-1]
        $request=@($events | Where-Object { $_.event -eq 'diff requested' -and $_.ms -le $tree.ms })[-1]
        if (-not $file -or -not $tree -or -not $request) { throw "Missing diff timing: $log" }
        [pscustomobject]@{ mode=$mode; sample=$sample; tabWorkerMs=$preview.ms-$requested.ms; pairTreeMs=$tree.ms-$request.ms; pairReadyMs=$file.ms-$request.ms }
    }
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $results=@()
    foreach ($mode in @('tab','pair')) {
        Run-One $mode -1 | Out-Null
        for ($i=0;$i -lt $Samples;$i++) { $results+=Run-One $mode $i }
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $results | Format-Table -AutoSize
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
}
