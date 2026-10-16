param(
    [string]$Candidate="$PSScriptRoot\..\target\release\git-rustle.exe",
    [int]$Samples=15,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference='Stop'
$Candidate=(Resolve-Path -LiteralPath $Candidate).Path
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory=(Resolve-Path -LiteralPath $OutputDirectory).Path
function Git([string]$root,[string[]]$arguments) {
    $value=& git.exe -C $root @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {throw "Git fixture command failed: $value"}
    ($value -join "`n").Trim()
}
function Write-Text([string]$path,[string]$value) {
    [IO.File]::WriteAllText($path,$value,[Text.UTF8Encoding]::new($false))
}
function Fixture([string]$name) {
    $root=Join-Path $OutputDirectory $name
    if (Test-Path -LiteralPath $root) {throw "Use a new output directory: $root"}
    New-Item -ItemType Directory -Path $root | Out-Null
    Git $root @('init','-q','-b','main') | Out-Null
    foreach ($pair in @(@('user.name','Pending Benchmark'),@('user.email','test@example.invalid'),@('commit.gpgSign','false'),@('core.autocrlf','false'),@('core.hooksPath','disabled-benchmark-hooks'))) {
        Git $root @('config',$pair[0],$pair[1]) | Out-Null
    }
    Write-Text (Join-Path $root '.git/rustle-pending-benchmark') 'Disposable pending benchmark fixture'
    foreach ($name in @('a.txt','b.txt','staged.txt')) {Write-Text (Join-Path $root $name) "base $name`n"}
    Git $root @('add','.') | Out-Null
    Git $root @('commit','-qm','Initial fixture') | Out-Null
    Write-Text (Join-Path $root 'a.txt') "updated a`n"
    Write-Text (Join-Path $root 'b.txt') "updated b`n"
    Write-Text (Join-Path $root 'staged.txt') "staged version`n"
    Git $root @('add','staged.txt') | Out-Null
    Write-Text (Join-Path $root 'staged.txt') "working version`n"
    Write-Text (Join-Path $root 'untracked.txt') "untracked version`n"
    $root
}
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
function Run-One([string]$mode,[int]$sample,[string]$shared) {
    $root=if ($mode -like 'commit-*') {Fixture "$mode-$sample-fixture"} else {$shared}
    $stem="$mode-$sample"
    $env:GIT_RUSTLE_DUMP=Join-Path $OutputDirectory "$stem-screen.txt"
    $log=Join-Path $OutputDirectory "$stem.txt"
    $prefix='xxxxxxxxxx<home>'
    switch ($mode) {
        'preview' {$env:GIT_RUSTLE_AUTOQUIT='450';Remove-Item Env:GIT_RUSTLE_KEYS -ErrorAction SilentlyContinue}
        'diff' {$env:GIT_RUSTLE_AUTOQUIT='1000';$env:GIT_RUSTLE_KEYS=$prefix+'d'}
        'commit-current' {$env:GIT_RUSTLE_AUTOQUIT='2400';$env:GIT_RUSTLE_KEYS=$prefix+'<tab>Bench<tab><tab><tab><tab><enter><tab><space><c-enter>'}
        'commit-new' {$env:GIT_RUSTLE_AUTOQUIT='3000';$env:GIT_RUSTLE_KEYS=$prefix+'<tab>Bench<tab>bench-pending<tab><tab><tab><enter><tab><space><c-enter>'}
    }
    $process=Start-Process -FilePath $Candidate -ArgumentList @('"'+$root+'"') -WindowStyle Hidden -RedirectStandardError $log -PassThru
    if (-not $process.WaitForExit(15000)) {Stop-Process -Id $process.Id;throw "Timed out: $log"}
    $process.Refresh()
    if ($process.ExitCode -ne 0) {throw "Native run failed: $log"}
    $timing=Get-Content $log -Raw
    $screen=Get-Content $env:GIT_RUSTLE_DUMP -Raw
    $parse={param($pattern) $m=[regex]::Match($timing,'([\d.]+) ms  '+$pattern);if(-not $m.Success){throw "Missing $pattern in $log"};[double]::Parse($m.Groups[1].Value,[cultureinfo]::InvariantCulture)}
    $requested=& $parse 'pending status requested'
    $ready=& $parse 'pending status ready \(4 files\)'
    $frame=& $parse 'pending changes frame'
    $commit=$null;$diff=$null
    if ($mode -eq 'preview') {
        foreach ($required in @('Pending changes','Commit changes','Modified files (4/4)','[x]','Check all','Uncheck all')) {if(-not $screen.Contains($required)){throw "Missing screen text $required in $log"}}
    } elseif ($mode -eq 'diff') {
        $diff=(& $parse 'file diff \(')-(& $parse 'file diff requested')
        if (-not $screen.Contains('updated a') -or -not $screen.Contains('Pending changes')) {throw "Wrong pending diff screen: $log"}
    } else {
        $commit=(& $parse 'commit done')-(& $parse 'commit requested')
        $expected=if($mode -eq 'commit-new'){'bench-pending'}else{'main'}
        if ((Git $root @('branch','--show-current')) -ne $expected) {throw "Wrong branch after commit: $log"}
        if ((Git $root @('show','HEAD:a.txt')) -ne 'updated a') {throw "Selected file not committed: $log"}
        if ((Git $root @('show','HEAD:b.txt')) -ne 'base b.txt') {throw "Unchecked file was committed: $log"}
        if ((Git $root @('show','HEAD:staged.txt')) -ne 'base staged.txt') {throw "Unchecked staged file was committed: $log"}
        if ((Git $root @('show',':staged.txt')) -ne 'staged version') {throw "Unchecked staging was changed: $log"}
        if ((Git $root @('log','-1','--format=%B')) -ne 'Bench') {throw "Commit description was not saved: $log"}
        if (-not (Test-Path (Join-Path $root 'untracked.txt'))) {throw "Untracked file was lost: $log"}
        if (-not $screen.Contains('Committed ')) {throw "Missing commit result: $log"}
    }
    [pscustomobject]@{mode=$mode;sample=$sample;statusWorkerMs=$ready-$requested;pendingFrameMs=$frame;commitWorkerMs=$commit;fileDiffMs=$diff}
}
try {
    $env:GIT_RUSTLE_TIMING='1'
    $shared=Fixture 'read-only-fixture'
    $results=@()
    foreach ($mode in @('preview','diff','commit-current','commit-new')) {
        Run-One $mode -1 $shared | Out-Null
        for ($n=0;$n -lt $Samples;$n++) {$results+=Run-One $mode $n $shared}
    }
    $results | ConvertTo-Json | Set-Content (Join-Path $OutputDirectory 'samples.json')
    $results | Format-Table -AutoSize
} finally {foreach ($name in $saved.Keys) {[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
