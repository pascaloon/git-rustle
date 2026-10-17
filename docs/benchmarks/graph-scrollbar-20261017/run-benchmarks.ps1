$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/graph-scrollbar-run.txt
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$fixture = Join-Path $benchRun 'scroll-fixture'
./scripts/new-scroll-fixture.ps1 -Path $fixture | Out-Null
$metadata = Get-Content (Join-Path $benchRun 'provenance.json') -Raw | ConvertFrom-Json
$metadata | Add-Member fixtureHead (git -C $fixture rev-parse HEAD)
$metadata | Add-Member fixtureCommits (git -C $fixture rev-list --all --count)
$metadata | Add-Member fixtureStatus @(git -C $fixture status --short)
$metadata | Add-Member candidateHash (Get-FileHash target/release/git-rustle.exe).Hash
$metadata | Add-Member candidateBytes (Get-Item target/release/git-rustle.exe).Length
$metadata | Add-Member sourceHashes @(Get-FileHash src/desktop.rs,src/desktop_text.rs,src/desktop_theme.rs | Select-Object Path,Hash)
$metadata | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $benchRun 'provenance.json')
git diff --binary --output="$benchRun/candidate.patch"
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $benchRepo -Baseline $baseline -Samples 15 -Scenarios @('scroll','graph-resize') -OutputDirectory (Join-Path $benchRun 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline -Samples 15 -Scenarios @('files-scroll','diff-scroll','diff-pan') -OutputDirectory (Join-Path $benchRun 'diff')
