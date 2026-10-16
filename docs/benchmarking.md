# Benchmark workflow for agents

Git Rustle prioritizes speed. Use this workflow to detect regressions before
delivering notable changes. Commands below run in PowerShell from the repository
root. This documents the workflow used for the remote UX and connection-dimming
changes on October 15, 2026; measurements are examples, not universal budgets.

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

There is no general scrolling/FPS or remote-file benchmark script yet. Add a
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
