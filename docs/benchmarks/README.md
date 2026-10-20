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

[`benchmark-conflicts.ps1`](../../scripts/benchmark-conflicts.ps1) bundles shared
startup/graph-frame controls and a preserved stash-operation worker with marked
merge/rebase/stash conflict UI workflows. It verifies actual commit/details
selection, resolution/save, external index edits observed by the watcher,
Continue and an Abort warning/action. Its companion
[`summarize-conflicts.ps1`](../../scripts/summarize-conflicts.ps1) retains named
operation intervals, candidate metadata/idle-probe costs and all shared gates.
See the [workflow](../benchmarking.md#conflict-inspection-and-live-refresh) for
fixture ownership, timing boundaries and limitations.

[`benchmark-tools-list.ps1`](../../scripts/benchmark-tools-list.ps1) focuses on
saved external-tool entries, one-time discovery and the Diff picker. It compares
startup/Diff/General controls and the existing launch worker, uses marked fresh
fake-install fixtures for native first-run actions, and measures list persistence
and migration separately. Its companion
[`summarize-tools-list.ps1`](../../scripts/summarize-tools-list.ps1) preserves all
samples, distinct timings and declared shared gates. See the
[workflow](../benchmarking.md#saved-external-tool-list) for fixture contracts and
measurement limits.

[`benchmark-todos.ps1`](../../scripts/benchmark-todos.ps1) bundles shared
startup/search/diff/scroll/settings controls with candidate graph navigation,
branch sorting/hover, stash, tools and reference-storage UI checks. It creates
marker-guarded repositories per mutating launch and isolates app-data/temp paths.
Shared worker comparisons and candidate operation components run sequentially
from preserved test executables. Its companion
[`summarize-todos.ps1`](../../scripts/summarize-todos.ps1) preserves backend,
branch-case, entry-count and workload groups, validates retained samples, and
writes the test-only activity-recency paired comparison separately. See the
[main workflow](../benchmarking.md#gpui-desktop-current-implementation) for
fixture contracts, safety boundaries and measurement limitations.

[`benchmark-tags-toolbar.ps1`](../../scripts/benchmark-tags-toolbar.ps1) is the
focused tag/count-slot bundle: shared startup/search/scroll, verified native
toolbar bounds, disposable tag-dialog/tag-create fixtures, and bounded shared plus
tag-specific operation workers. Its companion
[`summarize-tags-toolbar.ps1`](../../scripts/summarize-tags-toolbar.ps1) retains
absolute/percentage median and p95 deltas. See the main workflow for inputs,
fixture reuse and measurement limitations.

The parameterized [`benchmark-app-improvements.ps1`](../../scripts/benchmark-app-improvements.ps1)
bundles startup, graph/search/branches/status panels, focused search/provider
actions, controlled component measurements and fresh cached remote-preview delivery.
It accepts preserved executable paths and uses an isolated app-data directory.
Its companion [`summarize-app-improvements.ps1`](../../scripts/summarize-app-improvements.ps1)
can regenerate comparison tables without rerunning measurements. See the
[workflow recipe](../benchmarking.md#gpui-desktop-current-implementation) for
measurement boundaries, native input checks and limitations.

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
