$ErrorActionPreference='Stop'
$pendingRun=Get-Content target/bench/pending-changes-run.txt
$root=Join-Path $pendingRun 'unborn-complete-fixture'
if (Test-Path -LiteralPath $root) {throw 'Use a new fixture directory'}
New-Item -ItemType Directory -Path $root | Out-Null
git -C $root init -q -b main
foreach ($pair in @(@('user.name','Pending UI Test'),@('user.email','test@example.invalid'),@('commit.gpgSign','false'),@('core.autocrlf','false'),@('core.hooksPath','disabled-test-hooks'))) {
    git -C $root config $pair[0] $pair[1]
    if ($LASTEXITCODE -ne 0) {throw 'Fixture config failed'}
}
foreach ($name in @('a.txt','b.txt','unicode-é.txt')) {[IO.File]::WriteAllText((Join-Path $root $name),"first contents $name`n",[Text.UTF8Encoding]::new($false))}
$saved=@{}
foreach ($name in @('GIT_RUSTLE_TIMING','GIT_RUSTLE_AUTOQUIT','GIT_RUSTLE_KEYS','GIT_RUSTLE_DUMP')) {$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
function Launch([string]$label) {
    $env:GIT_RUSTLE_DUMP=Join-Path $pendingRun "$label-screen.txt"
    $exe=(Resolve-Path target/release/git-rustle.exe).Path
    $p=Start-Process -FilePath $exe -ArgumentList @('"'+$root+'"') -WindowStyle Hidden -RedirectStandardError (Join-Path $pendingRun "$label.txt") -PassThru
    if (-not $p.WaitForExit(15000)) {Stop-Process -Id $p.Id;throw 'Native validation timeout'}
    $p.Refresh()
    if ($p.ExitCode -ne 0) {throw 'Native validation failed'}
    Get-Content $env:GIT_RUSTLE_DUMP -Raw
}
try {
    $env:GIT_RUSTLE_TIMING='1';$env:GIT_RUSTLE_AUTOQUIT='400'
    Remove-Item Env:GIT_RUSTLE_KEYS -ErrorAction SilentlyContinue
    $before=Launch 'unborn-preview'
    if (-not $before.Contains('Commit changes') -or -not $before.Contains('Modified files (3/3)')) {throw 'Unborn pending form was not selected at startup'}
    $env:GIT_RUSTLE_AUTOQUIT='3200'
    $env:GIT_RUSTLE_KEYS='xxxxxxxxxx<home><tab>Initial commit<tab>initial-feature<c-enter>'
    $after=Launch 'complete-commit'
    $status=git -C $root status --porcelain
    if ($status) {throw "Working tree is still dirty: $status"}
    if ((git -C $root branch --show-current) -ne 'initial-feature') {throw 'New branch was not selected'}
    if ((git -C $root log -1 --format=%B) -ne 'Initial commit') {throw 'Wrong commit description'}
    if (@(git -C $root ls-tree -r --name-only HEAD).Count -ne 3) {throw 'Not all checked files were committed'}
    if ($after.Contains('Pending changes') -or $after.Contains('Commit changes') -or $after -notmatch '│ +Initial commit') {throw 'Completed commit was not selected or pending node was not removed'}
    [ordered]@{unbornPreviewSelected=$true;allCheckedCommitted=$true;cleanAfterCommit=$true;selectedCommitSubject='Initial commit';branch='initial-feature';head=(git -C $root rev-parse HEAD)} | ConvertTo-Json | Set-Content (Join-Path $pendingRun 'complete-validation.json')
    Get-Content (Join-Path $pendingRun 'complete-validation.json')
} finally {foreach($name in $saved.Keys){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
