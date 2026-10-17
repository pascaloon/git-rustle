# Benchmark workflow for agents

Git Rustle prioritizes speed. Use this workflow to detect regressions before
delivering notable changes. Commands below run in PowerShell from the repository
root. This documents the workflow used for the remote UX and connection-dimming
changes on October 15, 2026; measurements are examples, not universal budgets.


## GPUI desktop (current implementation)

The sections below this one describe the historical terminal harnesses. The
GPUI executable no longer consumes `GIT_RUSTLE_KEYS`, and its model dump is not
a screen capture. Do not use keyboard-script success as evidence for GPUI.

Preserve the baseline and finish release tests, Clippy and builds as described
below. For a terminal baseline and GPUI candidate use:

```powershell
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe `
  -NativeCandidate -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
.\scripts\benchmark-remote-preview.ps1 -Repository $fixturePath `
  -Baseline $baselineExe -NativeCandidate -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'preview')
.\scripts\benchmark-desktop.ps1 -Repository $benchRepo -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'desktop')
cargo test --release engine::tests::benchmark_desktop_mutations -- `
  --ignored --exact --nocapture
```

`-NativeCandidate` launches the GPUI window normally; the baseline console stays
hidden. For comparisons between two GPUI builds, also pass `-NativeBaseline`
to `benchmark.ps1`. Hiding the native window prevents a meaningful GPU frame measurement.
Keep the desktop unlocked and the app visible. Native windows appear briefly for
each sample. Do not compare a minimized/occluded GPU window with a visible one.
The standard startup script retains one warm-up and 15 alternating measured
launches per binary/cache mode. The preview fixture must be fresh enough for its
five-minute branch/tag caches; generate it only after builds finish.

The GPUI `first frame` mark is the next-frame callback following the first
qualifying history render. The old mark follows a console flush. These are
user-interface readiness proxies with different rendering pipelines, not equal
GPU/display presentation timestamps. Both exclude OS process-loader time.
GPUI platform, component and window setup have separate timing marks. The complete
walk remains a timestamp since main. A bootstrap worker drives local loading
while DirectX initializes; after handoff GPUI owns the engine. Network discovery
and worktree polling still wait until the first native frame.

The desktop suite runs scroll, branch activity, selected-file diff, metadata
search and settings-load scenarios through the same action methods as the UI.
Each scenario gets one discarded launch and 15 measured launches. Scroll collects
121 next-frame intervals and discards the first, saving 120 raw intervals per
launch. This includes event-loop scheduling, layout and paint submission; it
excludes input-device latency and does not measure scanout or claim display FPS.
The other intervals are worker request to result delivery. State summaries include
maximum visible list-row counts to detect a zero-height viewport. They are not a
substitute for visual or mouse/clipboard verification.

For scrollbar or diff rendering changes, also run `-Scenarios files-scroll,diff-scroll,diff-pan`
against a fixture with hundreds of changed files and a long, wide modified text file.
These scenarios collect the same 120 frame intervals while moving the file list,
the diff vertically, or both diff text panes horizontally. They do not measure mouse
input latency. The pre-scrollbar-fix executable lacks these scenarios: report them
as candidate-only measurements unless both executables include the instrumentation.

For search-decoration changes, also run `search-scroll` on a large history. It
waits for `m: merge` results, then scrolls 120 measured frames with highlights.
For selection changes, `compare` selects the first two graph rows using the same
normal-click/Ctrl-click action handlers and opens their diff. Use a clean fixture
with at least two commits so neither row is pending changes. This verifies action
dispatch, not physical modifier-key input. These scenarios require the current
instrumentation and must be reported as candidate-only against older baselines.

For pending changes, create a disposable repository and use
`-Scenarios pending,pending-diff`. The mutation component harness independently
creates disposable repositories, commits a selected change to a new branch and
checks out the original branch. It discards one warm-up, records 15 samples and
asserts success. Its status metric includes history startup; commit/checkout
metrics include worker dispatch and delivery but exclude actual mouse input and
GPU paint. The polling test harness has approximately 2 ms resolution. Never run
mutation fixtures against the user's benchmark clone.

Retain all raw data, executable hashes, limitations and failed/rejected runs.
For framework migrations, report the startup regression explicitly even when
scrolling remains fast; moving to GPU rendering does not imply faster startup.

## 1. Preserve a trustworthy baseline

Before editing code, build the current implementation and copy its release
executable somewhere the next build will not overwrite. Use a unique run folder:

```powershell
$ErrorActionPreference = 'Stop'
$benchRun = Join-Path $PWD ('target\bench\change-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
$benchRepo = Join-Path $env:TEMP 'gr-repos\git'
New-Item -ItemType Directory -Path $benchRun | Out-Null

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'Baseline build failed' }
$baselineExe = Join-Path $benchRun 'baseline.exe'
Copy-Item -LiteralPath target/release/git-rustle.exe -Destination $baselineExe
Get-FileHash -Algorithm SHA256 $baselineExe
git rev-parse HEAD
git status --short
rustc -Vv
```

Record the baseline SHA-256, Git revision, dirty working-tree state, compiler,
machine/CPU, and date in the report. A commit hash alone does not identify an
uncommitted implementation. Do not reset someone else's work to obtain a baseline.
If edits have already begun, use a previously preserved executable with known
provenance and disclose that choice. If no valid baseline exists, report absolute
measurements and the missing comparison instead of calling the candidate a baseline.

Verify the benchmark repository exists. The user's large clone is
`$env:TEMP\gr-repos\git`; earlier runs contained 85,593 commits and 1,021 refs.
Recheck the dataset rather than assuming those counts remain current:

```powershell
git -C $benchRepo rev-parse HEAD
git -C $benchRepo rev-list --all --count
@(git -C $benchRepo for-each-ref --format='%(refname)').Count
```

Keep its objects, refs, and worktree unchanged during comparisons. Checkout and
fetch tests belong in disposable fixtures. Application benchmark runs may write
Git Rustle's own caches; the overlay harness uses a dedicated cache identity.

## 2. Choose measurements that exercise the change

| Change | Measurements/checks |
| --- | --- |
| Loading, layout, cache, shared startup code | Cached and uncached first frame; complete uncached walk |
| Remote discovery or preview delivery | Native cached-preview comparison plus relevant provider fixtures |
| Overlay construction or edge styling | Large-repository overlay harness; time the changed component separately |
| Drawing, scrolling, popup or input behavior | Native interaction and screen checks; add repeatable action/frame timing if claiming interaction performance |
| Remote file lists or file diffs | Controlled provider fixture, request-to-ready interval, repeated/offline cache reuse |

The desktop suite measures scroll callback intervals, not display FPS. Remote-file
component fixtures are separate from native interaction measurements. Add a
focused measurement when needed; do not infer those speeds from startup results.
Use local HTTP fixtures for reproducible network behavior. Record live provider
latency separately, including host type and cache state, without logging tokens.

## 3. Validate and finish builds before timing

After implementing the change:

```powershell
cargo test --release
if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
cargo clippy --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'Candidate build failed' }
Get-FileHash -Algorithm SHA256 target/release/git-rustle.exe
```

Use optimized Windows builds for comparisons. Finish all builds/tests first;
never benchmark while another build, benchmark, or heavy workload is running.
Keep power mode, machine, terminal environment, and dataset comparable. Run
benchmark suites sequentially. If source changes after measurement, rebuild and
remeasure the affected paths before presenting results for the final executable.

## 4. Compare native startup

```powershell
.\scripts\benchmark.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'startup')
```

The script starts hidden native Windows consoles (`Start-Process -WindowStyle
Hidden`), checks process success, and collects stderr timing marks. For each
binary and mode it discards one warm-up, then records 15 measured launches,
alternating baseline/candidate order. It writes individual logs, `samples.json`,
and `summary.json`, and prints executable sizes.

- **First frame:** elapsed time from inside `main` to the first qualifying
  history frame after the console flush. It excludes OS process-loader time.
- **Complete walk:** the `walk done` timestamp since app start, not an isolated
  walker duration. It helps catch regressions hidden by a fast partial first frame.
- **Warm:** application caches enabled and warmed by the unmeasured launches.
- **Uncached:** `--no-cache` disables Git Rustle's disk cache reads/writes. It does
  not flush Windows filesystem caches or create a cold-boot measurement.

The script uses `GIT_RUSTLE_AUTOQUIT=10`. Do not use this short startup run to
measure remote readiness. Inspect logs for the expected row count, cache behavior,
and completed walk. Missing marks or errors invalidate a run; never treat missing
results as zero. The script restores its timing/key/dump environment variables.

## 5. Measure large-repository overlay work

```powershell
$previousBenchRepo = $env:GIT_RUSTLE_BENCH_REPO
try {
    $env:GIT_RUSTLE_BENCH_REPO = $benchRepo
    cargo test --release remote::history::tests::benchmark_overlay -- `
      --ignored --exact --nocapture |
      Tee-Object -FilePath (Join-Path $benchRun 'overlay.txt')
    if ($LASTEXITCODE -ne 0) { throw 'Overlay benchmark failed' }
} finally {
    $env:GIT_RUSTLE_BENCH_REPO = $previousBenchRepo
}
```

This opt-in harness walks the real local history once, adds synthetic chains of
100 and 1,000 remote commits in memory, and measures full overlay construction,
persistent overlay cache reads, and connection-style derivation separately.
Each component gets one warm-up and 15 measured samples. No Git objects or refs
are created for the synthetic commits. The harness reports retained style bytes
as well as times.

These timings exclude API requests, initial local loading, and UI installation.
The current harness measures components of the candidate; it does not execute
the baseline EXE. For a before/after component comparison, capture equivalent
harness results before editing too, with the same inputs and timing boundaries.
If Cargo recompiles during this command, let it finish and repeat the measured
run after the machine settles. The normal test suite skips this harness.

## 6. Compare cached remote-preview delivery

Build both binaries before generating the fixture. Choose a fresh disposable
directory and run the comparison within the five-minute branch/tag cache TTL:

```powershell
$previousFixture = $env:GIT_RUSTLE_GHOST_FIXTURE
try {
    $fixturePath = Join-Path $benchRun 'remote-fixture'
    $env:GIT_RUSTLE_GHOST_FIXTURE = $fixturePath
    cargo test --release remote::history::tests::prepare_native_fixture -- `
      --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'Fixture preparation failed' }

    .\scripts\benchmark-remote-preview.ps1 -Repository $fixturePath `
      -Baseline $baselineExe -Samples 15 `
      -OutputDirectory (Join-Path $benchRun 'preview')
} finally {
    $env:GIT_RUSTLE_GHOST_FIXTURE = $previousFixture
}
```

The fixture contains one local commit, two cached remote commits, tags, and cached
remote file data. Its provider hostname is deliberately unavailable, so successful
warm runs rely on disk caches. Both executables read the same fixture. Do not
point the generator at an existing repository: it initializes Git and adds a commit
and configuration. If the TTL expires, prepare a new uniquely named fixture.

The script uses one warm-up per binary, 15 alternating measured launches, and
`AUTOQUIT=250`. It requires `ghost preview ready (2 commits)` and reports:

- **Ready since startup:** first preview-ready timestamp since app start.
- **Worker interval:** preview-ready minus preview-requested timestamp; includes
  worker scheduling and result delivery, not just CPU time.

Report both. A worker-only improvement can hide slow discovery/configuration
before the request. Preview-ready is a result-delivery milestone, not a measured
screen flush. This tiny fixture does not replace the large-repository test or
establish first-time network performance. If cache formats or timing marks change,
adapt the comparison explicitly rather than silently using incompatible binaries.

## 7. Check the behavior being timed

Native interaction hooks live in `src/main.rs`:

- `GIT_RUSTLE_TIMING=1`: timestamps on stderr at exit.
- `GIT_RUSTLE_AUTOQUIT=<ms>`: automatic exit delay after the first frame, subject
  to the application's in-progress loading rules.
- `GIT_RUSTLE_KEYS=<script>`: scripted input after initial loading; named keys such
  as `<home>` and `<down>` are supported, with roughly 40 ms between events.
- `GIT_RUSTLE_DUMP=<absolute path>`: final screen as plain text.

Use hidden native-console launches for reproducibility and restore these variables
afterward. Screen dumps verify text/content, not colors; use `Screen::test_cells()`
regressions to assert glyph and foreground/background colors. For example, the
remote-diff check used `xxxxxxxxxx<home>d` and `AUTOQUIT=900` on the fresh fixture
to allow the preview to arrive before opening its first diff. `x` was an unbound
delay key; verify bindings before reusing it. `l` changes focus and is not a delay.

Provider fixture tests also verify pagination, failures, offline cache reuse, and
GitHub/GitLab behavior. They do not prove access to the user's on-premise GitLab.
Distinguish fixture validation from a live corporate-server test in the report.

## 8. Decide whether there is a regression

Compare like-for-like median and p95, plus absolute and percentage deltas. The
scripts use nearest-rank percentiles: with 15 samples, p95 is the largest sample.
This catches slow launches but is noisy; it is not a confidence interval.

There is no established universal percentage gate. Investigate repeatable
slowdowns, especially work added to startup or every redraw. A single 1–3% change
may be noise; an unexplained repeatable increase deserves investigation. When
results are ambiguous, inspect the raw logs, remove a known confounder, and repeat
the whole comparison in a new folder. Keep all runs and explain exclusions; do
not select only favorable samples or rerun until a number improves.

Time expensive stages separately to identify the cause, fix it, then rerun the
affected measurements on the final build. Background work still costs time and
memory: moving it off the UI thread does not make its latency irrelevant. Report
limits such as a synthetic chain covering less graph width than real merge-heavy
remote history. Do not claim statistical significance from these small samples.

## 9. Save evidence and a short decision log

Keep transient binaries and detailed console logs under the unique `target/bench`
folder. Copy the relevant raw JSON/text into `docs/benchmarks/<change>-*.json`
or `.txt`, and write `docs/<change>-benchmarks.md` with:

1. Problem/behavior changed and which performance paths could regress.
2. Date, environment, repository identity/counts, baseline and candidate hashes,
   source state, and executable sizes.
3. Exact commands, warm-ups, sample count/order, cache and network conditions.
4. Timing boundaries and exclusions; before/after table with median/p95/deltas.
5. Findings and corrective iterations, including rejected slow implementations.
6. Test/visual checks, limitations, and links to raw data.

Link the report from the README when useful. `target` artifacts are local and
may disappear; retained raw samples and the Markdown report are the durable
record. Documentation-only edits need link/command review, not new performance runs.

## Branch activity and remote action measurements

For branch browser changes, compare native entry frames and measure the activity
worker separately. The branch script records screen dumps as well as timing logs:

```powershell
.\scripts\benchmark-branches.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'branches')

$env:GIT_RUSTLE_BENCH_REPO = $benchRepo
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
$env:GIT_RUSTLE_BENCH_BRANCHES = '1000'
cargo test --release branches::tests::benchmark_browser -- --ignored --exact --nocapture
Remove-Item Env:GIT_RUSTLE_BENCH_BRANCHES
```

The component harness times worker scoring, browser installation/sorting, and
200 selection-plus-draw steps per sample on a 120×40 in-memory screen. It discards
one warm-up and retains 15 samples. The optional thousand-branch case uses the
first 1,000 commit IDs from `git rev-list --all` as synthetic branch tips without
writing refs. Drawing excludes terminal flushing and input delivery; native entry
frames are measured since startup and include the scripted 40 ms input delay.
The activity worker interval ends at message receipt, before sorting and drawing.

With a freshly prepared native fixture from section 6, measure the new actions:

```powershell
.\scripts\benchmark-remote-actions.ps1 -Repository $fixturePath -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'actions')
cargo test --release remote::files::tests::benchmark_remote_pairs -- `
  --ignored --exact --nocapture
```

The action script checks that remote-only Tab navigation installs three ghosts,
then separately opens a marked pair between divergent remote branches. It keeps
one warm-up and 15 samples per action and checks final screen contents. These
are candidate-only measurements because the older executable cannot perform the
actions. Tab times the preview worker request-to-delivery interval; pair times
the tree request to the tree result and to the selected file result. They exclude
scripted typing/delays and do not measure a final terminal flush.

The pair component harness creates 5,000 modified paths using a local HTTP server,
records the first tree and selected-file timings, then closes the server and
records one warm-up plus 15 persistent-cache samples. It exercises GitHub tree
listings; ordinary fixture tests cover both GitHub and GitLab behavior. The small
native fixture and the larger component fixture serve different purposes.

Restore any pre-existing benchmark environment variables after these commands.
Run these suites sequentially after all builds have finished, and retain their
raw output and limitations alongside startup and preview comparisons.

## Pending changes and commit measurements

Preserve a baseline before editing, complete release validation/builds, then run
the suites sequentially:

```powershell
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe `
  -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
.\scripts\benchmark-pending.ps1 -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'pending')
cargo test --release git::worktree::tests::benchmark_pending -- --ignored --exact --nocapture
```

If Cargo builds the component harness, discard that run and repeat after the
build finishes. The native pending suite creates disposable repositories under
its output directory, with staged, unstaged and untracked files. Each mode has
one discarded warm-up and 15 samples. Preview validates automatic startup focus
and composer controls. Diff opens a working-file diff. Commit modes create a
fresh fixture for every launch, uncheck all files, check one, and submit through
Ctrl+Enter on the current or a new branch. They verify HEAD contents, description,
branch, preservation of unchecked staged contents, and the untouched untracked file.
No checkout, staging or commits run in the user's large benchmark clone.

Status timing is worker request to result receipt; pending-frame time is since
main entry through the composer console flush. File-diff and commit intervals
run from request dispatch to result receipt, excluding scripted typing and delays.
The baseline cannot perform these new actions, so they are candidate-only.
The 1,000-file component fixture separately measures status, whole-tree checkbox
updates, one working-file diff, and mean selection/draw time over 200 frames per
sample. Drawing includes both graph and composer on a 120×40 in-memory screen,
excluding terminal flushing and input delivery. Retain raw samples and screens;
small fixtures do not establish large-worktree commit or live-hook latency.

## Startup HEAD selection measurements

After preserving a baseline and finishing release builds/tests, compare the
large repository's normal startup and then the off-screen HEAD fixture:

```powershell
.\scripts\benchmark.ps1 -Repository $benchRepo -Baseline $baselineExe `
  -Samples 15 -OutputDirectory (Join-Path $benchRun 'startup')
.\scripts\benchmark-startup-head.ps1 -Baseline $baselineExe `
  -Samples 15 -OutputDirectory (Join-Path $benchRun 'focus')
```

The focus script requires a new output directory. It creates 120 commits using
fast-import, checks out a branch at commit 20, then detaches HEAD at commit 10.
Both are below the initial graph viewport. Each state/cache mode discards one
warm-up per executable and retains 15 alternating native launches. No keys are
replayed. Screen dumps must show the candidate's details panel on HEAD and a
visible HEAD label; baseline dumps must show the newest commit. The first-frame
timing includes startup selection work when history and refs are ready, but is
not a separate milestone for HEAD becoming visible after later batch delivery.
State/order regressions cover late metadata, streaming and stale cache swaps.
This tiny fixture does not replace large-repository startup measurements or
establish scrolling FPS. All checkout/ref writes remain inside the new fixture.

## Graph label rendering measurements

For changes to graph reference labels, preserve the release executable and run
the component harness before editing production code, then repeat after final
builds and validation:

```powershell
$previousBenchRepo = $env:GIT_RUSTLE_BENCH_REPO
try {
    $env:GIT_RUSTLE_BENCH_REPO = $benchRepo
    cargo test --release app::tests::benchmark_graph_labels -- --ignored --exact --nocapture
} finally {
    $env:GIT_RUSTLE_BENCH_REPO = $previousBenchRepo
}
```

If Cargo compiled the harness, let the build finish and rerun for recorded
measurements. It loads the real local history once, measures label installation
separately, and draws 200 selection steps per sample near HEAD on a 120×40 screen
with 38 graph rows. It records one discarded warm-up plus 15 samples, reporting
mean selection/draw time per step. This excludes terminal flushing, input
delivery, network requests, and the initial history walk. Compare startup with
the preserved native executable separately and verify native screen contents.

## Graph refresh shortcut measurements

After preserving the baseline and completing builds/tests, compare the former
Graph `r` shortcut with the candidate's `F5`:

```powershell
.\scripts\benchmark-refresh.ps1 -Repository $benchRepo -Baseline $baselineExe `
  -Samples 15 -OutputDirectory (Join-Path $benchRun 'refresh')
```

The script runs hidden native consoles with warmed application caches, one
discarded warm-up per binary and 15 alternating samples. Ten unbound `x` keys
delay the refresh until startup discovery completes. Each log must contain two
completed discovery cycles; overlapping startup/manual discovery invalidates a
run. The measured interval is the manual `remote discovery requested` to
`remote discovery done`, including worker scheduling and delivery. This excludes
input delay, the separate local-ref reload, preview loading and screen flushing.
It also checks that the candidate's final screen contains the F5 hint. Use local
fixtures or separate measurements for provider/network latency claims. Compare
startup separately if shared input routing changed.

## Checkout confirmation measurements

After preserving the baseline and completing all release validation/builds:

```powershell
$checkoutFixture = Join-Path $benchRun 'checkout-fixture'
$previousCheckoutFixture = $env:GIT_RUSTLE_CHECKOUT_FIXTURE
try {
    $env:GIT_RUSTLE_CHECKOUT_FIXTURE = $checkoutFixture
    cargo test --release checkout::tests::prepare_native_fixture -- --ignored --exact --nocapture
    .\scripts\benchmark-checkout.ps1 -Repository $benchRepo -Fixture $checkoutFixture `
      -Baseline $baselineExe -Samples 15 -OutputDirectory (Join-Path $benchRun 'checkout')
} finally {
    $env:GIT_RUSTLE_CHECKOUT_FIXTURE = $previousCheckoutFixture
}
```

The fixture must be new. It has three commits, one changing text file, two local
branches, and a marker checked by the script. All confirmed checkouts and fixture
resets run there. Large-repository runs only open the popup, require its frame
and screen content, and reject any `checkout requested` mark.

Popup timing runs from the triggering Enter input to the first popup console
flush, including choice construction and the underlay draw. Graph branch/commit
checkouts have no baseline equivalent and are measured on the candidate only.
Browser branch checkout alternates both binaries: the candidate has an extra
confirmation Enter, while the worker interval excludes that user-input delay.
It measures `checkout requested` to `checkout done`, including Git subprocesses,
worker scheduling, and result delivery. HEAD and attached/detached state are
verified after each operation. One warm-up and 15 samples are used per mode/binary.
The script restores its environment and returns the disposable fixture to main.

These tiny-worktree results do not establish large-worktree or remote fetch
latency. Keep the native logs, screen dumps, raw JSON, and source/executable hashes
with the report; compare native startup separately when shared input/state changes.

## Graph search measurements

After preserving a baseline and completing release validation/builds, run startup
comparisons as above, then the search measurements sequentially:

```powershell
$env:GIT_RUSTLE_BENCH_REPO = $benchRepo
cargo test --release search::tests::benchmark_search -- --ignored --exact --nocapture |
  Tee-Object -FilePath (Join-Path $benchRun 'search-components.txt')
.\scripts\benchmark-search-fixture.ps1 -Repository $benchRepo `
  -OutputDirectory (Join-Path $benchRun 'search-fixture')
.\scripts\benchmark-search.ps1 `
  -Repository (Join-Path $benchRun 'search-fixture/repository') -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'search-native')
```

The component harness reports one initial sample followed by 15 repeated samples
for each field, including matching time and time through complete graph layout.
The initial message/file samples build their in-memory indexes without disk cache
I/O; these one-off samples are not p95 estimates. Paths are shared across commits.
The disposable native fixture has identical refs and shared read-only objects;
its local origin avoids live remote discovery changing the measured graph.

The native harness types each query at 40 ms intervals, then presses Enter. It
records final-keystroke-to-interactive-results-frame and final-keystroke-to-complete-
graph-frame separately, plus the entire first-keystroke-to-final-frame interval
(which includes typing). Each operation has one warm-up plus 15 measured launches.
Persistent search caches are enabled. Keep the warm-up logs: the first file search
on a previously unindexed repository can take much longer than subsequent queries.
Autoquit waits for search completion. Inspect dumps for the query, result count,
filtered commit rows and selected details, including an empty-result case.

Do not describe match availability as complete graph latency or use warm queries
to characterize first-time indexing. Provider/API file-list latency requires a
separate fixture or explicitly identified live measurement.

## Worked decision log: connection dimming

The [connection-dimming report](connection-dimming-benchmarks.md) records this
actual iteration:

1. Preserved the previous release as `connection-dimming-baseline.exe`.
2. Fixed edge provenance and added regression tests for merges, crossings,
   intervening local rows, color reuse, and cached preview styling.
3. Measured an initial two-pass implementation on the large repository. Scanning
   all 17.3 million graph cells cost 34–43 ms and retained roughly 17.3 MB of flags.
   That cost was rejected despite the work running on a background thread.
4. Limited traversal to affected connections and stopped at their endpoints.
   Final medians were 0.834/0.897 ms for 100/1,000 added commits, retaining
   21,657/22,557 flag bytes. Scrolling reads the precomputed flags.
5. Rebuilt and compared final native startup: cached median 12.197 → 12.112 ms;
   uncached median 13.428 → 13.643 ms, with p95 essentially unchanged. Reported
   the small increase rather than claiming every metric improved. All 74 release
   tests passed; Clippy was clean.

The earlier [remote UX report](remote-ux-benchmarks.md) also caught a redundant
configuration lookup during intermediate measurement. Its final comparison
records cached preview readiness improving from 83.290 to 45.268 ms, alongside
the worker interval, so the reported benefit reflects the complete startup path.

Other historical reports: [remote support](remote-benchmarks.md),
[settings](settings-benchmarks.md), and [ghost commits](ghost-benchmarks.md).
