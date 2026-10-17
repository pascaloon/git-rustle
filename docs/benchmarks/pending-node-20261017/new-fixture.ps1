param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
if (Test-Path -LiteralPath $Path) { throw 'Choose a new fixture directory' }
New-Item -ItemType Directory -Path $Path | Out-Null
$Path = (Resolve-Path -LiteralPath $Path).Path
function Git-Fixture([string[]]$GitArgs) {
    & git -C $Path @GitArgs
    if ($LASTEXITCODE -ne 0) { throw 'Fixture Git command failed' }
}
Git-Fixture @('init','-q','-b','main')
Git-Fixture @('config','user.name','Pending Node Benchmark')
Git-Fixture @('config','user.email','pending@example.invalid')
Git-Fixture @('config','commit.gpgSign','false')
Git-Fixture @('config','core.hooksPath','disabled-benchmark-hooks')
Git-Fixture @('config','core.autocrlf','false')
$oldAuthor = $env:GIT_AUTHOR_DATE; $oldCommitter = $env:GIT_COMMITTER_DATE
try {
    foreach ($entry in @(@('base','00'),@('main','01'),@('other','02'))) {
        if ($entry[0] -eq 'other') { Git-Fixture @('switch','-q','-c','other','HEAD~1') }
        [IO.File]::WriteAllText((Join-Path $Path 'file.txt'), $entry[0] + "`n")
        $env:GIT_AUTHOR_DATE = "2026-10-15T12:$($entry[1]):00+00:00"
        $env:GIT_COMMITTER_DATE = $env:GIT_AUTHOR_DATE
        Git-Fixture @('add','file.txt')
        Git-Fixture @('commit','-qm',$entry[0])
    }
    Git-Fixture @('switch','-q','main')
    [IO.File]::WriteAllText((Join-Path $Path 'file.txt'), "pending`n")
} finally {
    $env:GIT_AUTHOR_DATE = $oldAuthor; $env:GIT_COMMITTER_DATE = $oldCommitter
}
Write-Output $Path
