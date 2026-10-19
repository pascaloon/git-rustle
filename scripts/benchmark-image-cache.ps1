param(
    [string]$Candidate = "$PSScriptRoot\..\target\release\git-rustle.exe",
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$Image,
    [int]$Samples = 15
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$OutputDirectory = (Resolve-Path $OutputDirectory).Path
$repo = Join-Path $OutputDirectory 'repository'
if (Test-Path $repo) { throw 'Use a fresh output directory for the cache-control fixture' }
$appdata = Join-Path $OutputDirectory 'appdata'
$seed = Join-Path $OutputDirectory 'seed'
$hooks = Join-Path $OutputDirectory 'empty-hooks'
New-Item -ItemType Directory -Force $repo,$appdata,$seed,$hooks | Out-Null
[IO.File]::WriteAllText((Join-Path $appdata 'cache-benchmark-fixture'), 'disposable image cache benchmark')
$bytes = [IO.File]::ReadAllBytes((Resolve-Path $Image).Path)
for ($i=0; $i -lt 500; $i++) {
    $hash = $i.ToString('x40')
    [IO.File]::WriteAllBytes((Join-Path $seed "image-$hash.bin"), $bytes)
    [IO.File]::WriteAllText((Join-Path $seed "author-$hash.bin"), 'https://avatars.example/cache-fixture.png')
}
git -C $repo init -q -b main
if ($LASTEXITCODE) { throw 'Fixture initialization failed' }
git -C $repo -c user.name=Benchmark -c user.email=benchmark@example.invalid -c commit.gpgsign=false -c "core.hooksPath=$hooks" commit --allow-empty -qm 'Cache controls fixture'
if ($LASTEXITCODE) { throw 'Fixture commit failed' }
$previousLocal = $env:LOCALAPPDATA
$previousRecents = $env:GIT_RUSTLE_RECENTS_FILE
try {
    $env:LOCALAPPDATA = $appdata
    $env:GIT_RUSTLE_RECENTS_FILE = Join-Path $OutputDirectory 'recents.json'
    & "$PSScriptRoot/benchmark-desktop.ps1" -Repository $repo -Candidate $Candidate -Samples $Samples -Scenarios @('settings-cache','settings-cache-clear') -ImageCacheSeed $seed -RejectCompetingBuilds -OutputDirectory (Join-Path $OutputDirectory 'measurements')
} finally {
    $env:LOCALAPPDATA = $previousLocal
    $env:GIT_RUSTLE_RECENTS_FILE = $previousRecents
}
