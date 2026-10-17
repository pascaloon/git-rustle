$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/checkboxes-run.txt
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$pending = Join-Path $benchRun 'pending-fixture-native'
$diff = Join-Path $benchRun 'diff-fixture-native'
$metadata = Get-Content (Join-Path $benchRun 'provenance.json') -Raw | ConvertFrom-Json
$metadata | Add-Member candidateHash (Get-FileHash target/release/git-rustle.exe).Hash
$metadata | Add-Member candidateBytes (Get-Item target/release/git-rustle.exe).Length
$metadata | Add-Member sourceHashes @(Get-FileHash src/desktop.rs,src/desktop_icons.rs,src/diff/changes.rs | Select-Object Path,Hash)
$metadata | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $benchRun 'provenance.json')
git diff --binary --output="$benchRun/candidate.patch"
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $pending -Baseline $baseline -Samples 15 -Scenarios @('files-scroll') -OutputDirectory (Join-Path $benchRun 'pending')
./scripts/benchmark-desktop.ps1 -Repository $diff -Baseline $baseline -Samples 15 -Scenarios @('diff-scroll','diff-pan') -OutputDirectory (Join-Path $benchRun 'diff')
