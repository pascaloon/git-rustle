$ErrorActionPreference = 'Stop'
$benchRun = Get-Content target/repo-header-run.txt
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$candidate = Join-Path $PWD 'target/release/git-rustle.exe'
$metadata = Get-Content (Join-Path $benchRun 'provenance.json') -Raw | ConvertFrom-Json
$metadata | Add-Member candidateHash (Get-FileHash $candidate).Hash
$metadata | Add-Member candidateBytes (Get-Item $candidate).Length
$metadata | Add-Member sourceHashes @(Get-FileHash src/git/mod.rs,src/engine.rs | Select-Object Path,Hash)
$metadata | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $benchRun 'provenance.json')
git diff --binary --output="$benchRun/candidate.patch"
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline -Candidate $candidate -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'absolute')
& (Join-Path $benchRun 'benchmark-relative.ps1') -Repository $benchRepo -Baseline $baseline -Candidate $candidate -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'relative')
