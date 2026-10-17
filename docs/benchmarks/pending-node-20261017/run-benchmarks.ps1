$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/pending-node-run.txt
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$fixture = Join-Path $benchRun 'pending-fixture-native'
$metadata = Get-Content (Join-Path $benchRun 'provenance.json') -Raw | ConvertFrom-Json
$metadata | Add-Member candidateHash (Get-FileHash target/release/git-rustle.exe).Hash
$metadata | Add-Member candidateBytes (Get-Item target/release/git-rustle.exe).Length
$metadata | Add-Member sourceHashes @(Get-FileHash src/desktop.rs,src/engine.rs,src/git/mod.rs | Select-Object Path,Hash)
$metadata | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $benchRun 'provenance.json')
git diff --binary --output="$benchRun/candidate.patch"
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $benchRepo -Baseline $baseline -Samples 15 -Scenarios @('scroll') -OutputDirectory (Join-Path $benchRun 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline -Samples 15 -Scenarios @('graph-resize') -OutputDirectory (Join-Path $benchRun 'pending')
