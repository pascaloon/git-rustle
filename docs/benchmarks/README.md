# Benchmark helpers

Start with [the benchmark workflow](../benchmarking.md). The general harnesses
and scroll fixture generator live in [`scripts/`](../../scripts). This directory
keeps additional scenario runners, fixture generators and summary scripts.
The [command recipes](../benchmark-recipes.md) retain reproduction commands from
previous reports.
Generated results and historical reports are excluded from version control;
previous results can be recovered from Git history.

Run commands from the repository root. Store new binaries, fixtures, provenance,
samples, logs, screenshots and `report.md` under a unique `target/bench/<run>`
directory. Preserve the baseline before making application changes, finish builds
before timing, and run suites sequentially on an unlocked desktop.

## Additional scenario scripts

Each dated directory below contains `run-benchmarks.ps1` and `summarize.ps1`.
These are preserved reproduction recipes with setup assumptions, rather than
standalone replacements for the parameterized harnesses in `scripts/`.

| Directory | Measurements and fixtures |
| --- | --- |
| [checkboxes-20261017](checkboxes-20261017) | Startup, pending file-list scrolling and diff scrolling/panning; includes `new-pending-fixture.ps1`. |
| [details-contrast-20261017](details-contrast-20261017) | Startup, history/search scrolling, tabs and file-list scrolling. |
| [graph-scrollbar-20261017](graph-scrollbar-20261017) | Startup, graph scrolling/resizing and file/diff scrolling/panning. |
| [oled-theme-20261017](oled-theme-20261017) | Startup, history/search scrolling and file/diff scrolling/panning. |
| [pending-node-20261017](pending-node-20261017) | Startup, history scrolling and pending graph resizing; includes `new-fixture.ps1`. |
| [repo-header-20261017](repo-header-20261017) | Absolute/relative repository-path startup and repository-ready timing; includes `benchmark-relative.ps1`. |
| [sidebar-theme-20261017](sidebar-theme-20261017) | Startup, history scrolling, tabs and file-list scrolling. |

The runners read a local `target/<scenario>-run.txt` file containing the absolute
run directory. That directory must contain `baseline.exe` and `provenance.json`
with baseline/environment metadata from the workflow. The candidate is
`target/release/git-rustle.exe`; the large history is `$env:TEMP/gr-repos/git`.
The summary scripts read the runner's output from the same run directory.

For example, after preserving a baseline and recording its metadata in `$benchRun`:

```powershell
$benchRun | Set-Content target/graph-scrollbar-run.txt
./docs/benchmarks/graph-scrollbar-20261017/run-benchmarks.ps1
./docs/benchmarks/graph-scrollbar-20261017/summarize.ps1
```

Most runners create their scroll fixture using `scripts/new-scroll-fixture.ps1`.
The checkbox runner instead needs both fixtures prepared beforehand:

```powershell
./docs/benchmarks/checkboxes-20261017/new-pending-fixture.ps1 `
  -Path (Join-Path $benchRun 'pending-fixture-native')
./scripts/new-scroll-fixture.ps1 -Path (Join-Path $benchRun 'diff-fixture-native')
```

The pending-node runner needs its own fixture prepared beforehand:

```powershell
./docs/benchmarks/pending-node-20261017/new-fixture.ps1 `
  -Path (Join-Path $benchRun 'pending-fixture-native')
```

The repository-header runner expects a copy of `benchmark-relative.ps1` inside
the run directory. It passes explicit candidate and output paths; pass those
paths explicitly when invoking that helper directly as well.

```powershell
Copy-Item docs/benchmarks/repo-header-20261017/benchmark-relative.ps1 `
  -Destination (Join-Path $benchRun 'benchmark-relative.ps1')
```

## Legacy terminal verification

[head-label-check-native.ps1](head-label-check-native.ps1) reads
`target/bench/head-label-run.txt` and checks attached/detached HEAD screen dumps
using a disposable checkout fixture and the preserved baseline.
[pending-changes-verify-complete.ps1](pending-changes-verify-complete.ps1) reads
`target/bench/pending-changes-run.txt` and creates a disposable unborn repository
to verify complete commit contents.

These two scripts use the former terminal input and screen-dump instrumentation.
The GPUI executable does not consume scripted terminal keys, so they require a
compatible terminal build and cannot validate current desktop interaction.
