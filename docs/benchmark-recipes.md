# Benchmark command recipes

Reproduction commands retained from previous benchmark reports. Follow the
[current workflow](benchmarking.md) for baseline preservation, fixture safety,
measurement limits and recording results under ignored `target/bench/<run>`.
The [helper guide](benchmarks/README.md) documents additional scenario scripts.

These recipes use variables and executable paths from their original runs.
Set them to a fresh local run directory, your benchmark repository, preserved
baseline and current candidate. Finish builds before collecting measurements.
Terminal key/screen recipes require a compatible terminal build; they do not
validate the GPUI desktop. Use the desktop scenarios in the current workflow.

## branch improvements

```powershell
cargo test --release
cargo clippy --all-targets -- -D warnings
cargo build --release
$env:GIT_RUSTLE_BENCH_REPO = $benchRepo
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
$env:GIT_RUSTLE_BENCH_BRANCHES = '1000'
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
Remove-Item Env:GIT_RUSTLE_BENCH_BRANCHES
cargo test --release remote::history::tests::benchmark_overlay -- --ignored --exact --nocapture
cargo test --release remote::files::tests::benchmark_remote_pairs -- --ignored --exact --nocapture
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
.\scripts\benchmark-branches.ps1 -Repository $benchRepo -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'branches')
$fixturePath = Join-Path $benchRun 'remote-fixture'
$env:GIT_RUSTLE_GHOST_FIXTURE = $fixturePath
cargo test --release remote::history::tests::prepare_native_fixture -- --ignored --exact --nocapture
.\scripts\benchmark-remote-preview.ps1 -Repository $fixturePath -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'preview')
.\scripts\benchmark-remote-actions.ps1 -Repository $fixturePath -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'actions')
```

## branch polish

```powershell
# Run this pair on both baseline sources, before editing, and final sources.
$env:GIT_RUSTLE_BENCH_REPO = $benchRepo
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
$env:GIT_RUSTLE_BENCH_BRANCHES = '1000'
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
Remove-Item Env:GIT_RUSTLE_BENCH_BRANCHES

# Finish all validation/builds before these comparisons.
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
.\scripts\benchmark-branches.ps1 -Repository $benchRepo -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'branches')
```

## checkout confirmation

```powershell
cargo clippy --all-targets -- -D warnings
cargo test --release
cargo build --release
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
$env:GIT_RUSTLE_CHECKOUT_FIXTURE = Join-Path $benchRun 'fixture'
cargo test --release checkout::tests::prepare_native_fixture -- --ignored --exact --nocapture
.\scripts\benchmark-checkout.ps1 -Repository $benchRepo `
  -Fixture $env:GIT_RUSTLE_CHECKOUT_FIXTURE -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'checkout')
```

## details contrast

```powershell
$benchRun = 'target/bench/details-contrast-20261017-012956'
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$fixture = Join-Path $benchRun 'scroll-fixture-native'
./scripts/new-scroll-fixture.ps1 -Path $fixture
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline `
  -NativeBaseline -NativeCandidate -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $benchRepo -Baseline $baseline `
  -Samples 15 -Scenarios @('scroll','search-scroll','tabs') `
  -OutputDirectory (Join-Path $benchRun 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline `
  -Samples 15 -Scenarios @('files-scroll') `
  -OutputDirectory (Join-Path $benchRun 'diff')
```

## ghost

```powershell
.\scripts\benchmark.ps1 -Repository "$env:TEMP\gr-repos\git" `
  -Baseline .\target\bench\ghost-baseline.exe -Samples 15 `
  -OutputDirectory .\target\bench\ghost-final-comparison
```

```powershell
$env:GIT_RUSTLE_BENCH_REPO = "$env:TEMP\gr-repos\git"
cargo test --release benchmark_overlay -- --ignored --nocapture
```

```powershell
$env:GIT_RUSTLE_GHOST_FIXTURE = "$PWD\target\ghost-fixture"
cargo test --release prepare_native_fixture -- --ignored --nocapture
# Launch this fixture within five minutes to use its fresh metadata caches.
```

## gpui tabs

```powershell
$run = 'F:\Projects\git-rustle\target\bench\gpui-tabs-20261017-002441'
$repo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $run 'baseline.exe'
$candidate = Join-Path $run 'candidate.exe'
$fixture = Join-Path $run 'fixture'
# Fixture created once with scripts/new-scroll-fixture.ps1 -Path $fixture.
./scripts/benchmark.ps1 -Repository $repo -Baseline $baseline -Candidate $candidate -NativeBaseline -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $run 'startup')
./scripts/benchmark-desktop.ps1 -Repository $repo -Baseline $baseline -Candidate $candidate -Samples 15 -Scenarios @('scroll','search-scroll','branches','settings') -OutputDirectory (Join-Path $run 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline -Candidate $candidate -Samples 15 -Scenarios @('files-scroll','diff-scroll','diff-pan','diff','compare') -OutputDirectory (Join-Path $run 'diff')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Candidate $candidate -Samples 15 -Scenarios @('wrap-scroll','tabs') -OutputDirectory (Join-Path $run 'new-diff')
./scripts/benchmark-desktop.ps1 -Repository $repo -Candidate $candidate -Samples 15 -Scenarios @('graph-resize') -OutputDirectory (Join-Path $run 'resize')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline -Candidate $candidate -Samples 15 -Scenarios @('diff','compare') -OutputDirectory (Join-Path $run 'diff-repeat')
```

## graph refresh

```powershell
$run = Get-Content target/bench/graph-refresh-run.txt
$repo = Join-Path $env:TEMP 'gr-repos/git'
.\scripts\benchmark-refresh.ps1 -Repository $repo -Baseline "$run/baseline.exe" `
  -Samples 15 -OutputDirectory "$run/refresh"
.\scripts\benchmark.ps1 -Repository $repo -Baseline "$run/baseline.exe" `
  -Samples 15 -OutputDirectory "$run/startup"
```

## graph search

```powershell
cargo test --release
cargo clippy --all-targets -- -D warnings
cargo build --release
$env:GIT_RUSTLE_BENCH_REPO = "$env:TEMP/gr-repos/git"
cargo test --release search::tests::benchmark_search -- --ignored --exact --nocapture
.\scripts\benchmark-search-fixture.ps1 -OutputDirectory "$benchRun/cold"
.\scripts\benchmark-search.ps1 -Repository "$benchRun/cold/repository" `
  -Operations files -Samples 1 -OutputDirectory "$benchRun/native-cold"
.\scripts\benchmark-search-fixture.ps1 -OutputDirectory "$benchRun/controlled"
.\scripts\benchmark-search.ps1 -Repository "$benchRun/controlled/repository" `
  -Samples 15 -OutputDirectory "$benchRun/native"
.\scripts\benchmark.ps1 -Repository "$benchRun/controlled/repository" `
  -Baseline $baselineExe -Samples 15 -OutputDirectory "$benchRun/startup"
```

## head label

```powershell
$run = Get-Content target/bench/head-label-run.txt
$repo = Join-Path $env:TEMP 'gr-repos/git'
$previousBenchRepo = $env:GIT_RUSTLE_BENCH_REPO
try {
    $env:GIT_RUSTLE_BENCH_REPO = $repo
    cargo test --release app::tests::benchmark_graph_labels -- --ignored --exact --nocapture
} finally {
    $env:GIT_RUSTLE_BENCH_REPO = $previousBenchRepo
}
.\scripts\benchmark.ps1 -Repository $repo -Baseline "$run/baseline.exe" `
  -Samples 15 -OutputDirectory "$run/startup"
```

## oled theme

```powershell
$benchRun = 'target/bench/oled-theme-20261017-010737'
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$fixture = Join-Path $benchRun 'scroll-fixture-native'
./scripts/new-scroll-fixture.ps1 -Path $fixture
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline `
  -NativeBaseline -NativeCandidate -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $benchRepo -Baseline $baseline `
  -Samples 15 -Scenarios @('scroll','search-scroll') `
  -OutputDirectory (Join-Path $benchRun 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline `
  -Samples 15 -Scenarios @('files-scroll','diff-scroll','diff-pan') `
  -OutputDirectory (Join-Path $benchRun 'diff')
```

## pending changes

```powershell
$run = Get-Content target/bench/pending-changes-run.txt
cargo clippy --all-targets -- -D warnings
cargo test --release
cargo build --release
.\scripts\benchmark-pending.ps1 -Samples 15 -OutputDirectory "$run/pending-final"
.\scripts\benchmark.ps1 -Repository "$env:TEMP/gr-repos/git" `
  -Baseline "$run/baseline.exe" -Samples 15 -OutputDirectory "$run/startup-final"
cargo test --release git::worktree::tests::benchmark_pending -- --ignored --exact --nocapture
```

## remote

```powershell
.\scripts\benchmark.ps1 -Repository "$env:TEMP\gr-repos\git" `
  -Baseline .\target\bench\baseline.exe -Samples 15
```

## remote ux

```powershell
cargo test --release
cargo build --release
.\scripts\benchmark.ps1 -Repository "$env:TEMP\gr-repos\git" `
  -Baseline .\target\bench\ghost-fixes-baseline.exe -Samples 15 `
  -OutputDirectory .\target\bench\remote-ux-final-comparison

$env:GIT_RUSTLE_BENCH_REPO = "$env:TEMP\gr-repos\git"
cargo test --release remote::history::tests::benchmark_overlay -- --ignored --exact --nocapture

# Choose a new directory; prepare and measure within the five-minute cache TTL.
$env:GIT_RUSTLE_GHOST_FIXTURE = "$PWD\target\remote-preview-fixture"
cargo test --release remote::history::tests::prepare_native_fixture -- --ignored --exact --nocapture
.\scripts\benchmark-remote-preview.ps1 -Repository $env:GIT_RUSTLE_GHOST_FIXTURE `
  -Baseline .\target\bench\ghost-fixes-baseline.exe -Samples 15
```

## search partial

```powershell
cargo test search::tests
cargo test --release
cargo clippy --all-targets -- -D warnings
cargo build --release
.\scripts\benchmark-search.ps1 -Repository $fixtureRepo `
  -Candidate "$partialRun/baseline.exe" -Operations @('partial','words') `
  -Samples 15 -OutputDirectory "$partialRun/native-baseline"
.\scripts\benchmark-search.ps1 -Repository $fixtureRepo `
  -Candidate "$partialRun/candidate.exe" -Operations @('partial','words') `
  -Samples 15 -OutputDirectory "$partialRun/native-candidate"
.\scripts\benchmark.ps1 -Repository $fixtureRepo `
  -Baseline "$partialRun/baseline.exe" -Candidate "$partialRun/candidate.exe" `
  -Samples 15 -OutputDirectory "$partialRun/startup"
```

## search words

```powershell
cargo test --release
cargo clippy --all-targets -- -D warnings
cargo build --release
$env:GIT_RUSTLE_BENCH_REPO = $fixtureRepo
cargo test --release search::tests::benchmark_search -- --ignored --exact --nocapture
.\scripts\benchmark-search.ps1 -Repository $fixtureRepo `
  -Candidate "$wordRun/baseline.exe" -Operations @('default','message','words') `
  -Samples 15 -OutputDirectory "$wordRun/native-baseline"
.\scripts\benchmark-search.ps1 -Repository $fixtureRepo `
  -Candidate "$wordRun/candidate.exe" -Operations @('default','message','words') `
  -Samples 15 -OutputDirectory "$wordRun/native-candidate"
.\scripts\benchmark.ps1 -Repository $fixtureRepo `
  -Baseline "$wordRun/baseline.exe" -Candidate "$wordRun/candidate.exe" `
  -Samples 15 -OutputDirectory "$wordRun/startup"
```

## settings

```powershell
.\scripts\benchmark.ps1 -Repository "$env:TEMP\gr-repos\git" `
  -Baseline .\target\bench\settings-baseline.exe -Samples 15 `
  -OutputDirectory .\target\bench\settings-comparison
```

## sidebar theme

```powershell
$benchRun = 'target/bench/sidebar-theme-20261017-121832'
$benchRepo = Join-Path $env:TEMP 'gr-repos/git'
$baseline = Join-Path $benchRun 'baseline.exe'
$fixture = Join-Path $benchRun 'scroll-fixture-native'
./scripts/new-scroll-fixture.ps1 -Path $fixture
./scripts/benchmark.ps1 -Repository $benchRepo -Baseline $baseline `
  -NativeBaseline -NativeCandidate -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
./scripts/benchmark-desktop.ps1 -Repository $benchRepo -Baseline $baseline `
  -Samples 15 -Scenarios @('scroll','tabs') `
  -OutputDirectory (Join-Path $benchRun 'history')
./scripts/benchmark-desktop.ps1 -Repository $fixture -Baseline $baseline `
  -Samples 15 -Scenarios @('files-scroll') `
  -OutputDirectory (Join-Path $benchRun 'diff')
```

## startup head

```powershell
$run = Get-Content target/bench/startup-head-run.txt
.\scripts\benchmark.ps1 -Repository "$env:TEMP/gr-repos/git" `
  -Baseline "$run/baseline.exe" -Samples 15 -OutputDirectory "$run/startup"
.\scripts\benchmark-startup-head.ps1 -Baseline "$run/baseline.exe" `
  -Samples 15 -OutputDirectory "$run/focus-3"
```
