# Benchmark workflow for agents

Git Rustle prioritizes speed. Use this workflow to detect regressions before
delivering notable changes. Commands below run in PowerShell from the repository
root. Keep benchmark instructions, scripts and harness code in version control;
write generated results to unique ignored `target/bench/<run>` directories.
Additional scenario-specific scripts and their setup requirements are listed in
[the benchmark helper guide](benchmarks/README.md).


## GPUI desktop (current implementation)

For the graph/search/provider usability bundle, use the sequential
[app-improvements runner](../scripts/benchmark-app-improvements.ps1). Preserve the
application baseline before editing and finish every build first. Pass the final
release test executable directly so fixture/component measurements cannot compile:

```powershell
.\scripts\benchmark-app-improvements.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

This groups native startup (warm/uncached), large-history scroll/search-scroll,
branch activity, search delivery and status-panel rendering against the baseline.
It then measures candidate-only search focus in Graph/Branches, clearing single
and multiple selections, provider-badge scrolling and the provider context menu.
Search-clear scenarios use actual input subscriptions and selection handlers,
render a different viewport before clearing, and assert the last selected commit,
retained multiple selection and actual viewport visibility after the final frame.
The native fixture needs at least three commits matching `m: merge`; the multiple
selection scenario also toggles its latest row off before clearing. These focus
measurements dispatch Ctrl+F through the native keyboard listeners and assert
the target input focus; verify physical keyboard input separately. Header photo/badge
fixtures require an observed badge render and decoded header image, exercising
cached rendering without public-provider traffic. Hover tooltips, actual pointer menus and browser
opening also need native visual/input checks.

The runner uses isolated app data under its output directory, rejects competing
builds, retains raw stdout/frames/model dumps and hashes, and generates
`summary.json`/`summary.md`. `-Stages Startup,History,Actions,Components,Preview`
can select unfinished stages; existing stage directories are never overwritten.
The cached remote-preview fixture is generated immediately before its comparison
to respect the five-minute cache TTL. The default component measurement is
`remote::history::tests::benchmark_tag_refresh`: one warm-up and 15 samples for
each provider's cached tags, first/repeated HTTP 502 retention and later HTTP 200
recovery. It asserts cached publication before the request, unchanged cache bytes
on failures and unchanged refs. Loopback timings include test-server scheduling;
they do not characterize public API latency or an automatic retry policy.
The second default harness, `remote::requests::integration::tests::benchmark_provider_cache`,
measures one warm-up and 15 warm disk reads/parses/validations of a 10,000-request
JSON cache; it does not measure project metadata or HTTP requests.
`desktop::provider::tests::benchmark_provider_lookup` separately measures
populated branch-to-request lookups without layout or network work.
Extra ignored component tests can be supplied with `-ComponentTests`.

The Components stage also runs
[`benchmark-provider-fixture.ps1`](../scripts/benchmark-provider-fixture.ps1)
against the already-built candidate test executable. It discards one warm-up and
records 15 complete process durations for
`remote::requests::integration::tests::provider_pagination_project_icons_and_offline_cache_reuse`,
requiring exactly one successful test in every launch. The fixture covers both
providers' project-icon metadata, request pagination, cache persistence and offline
cache reuse. Its elapsed time includes process launch/loader, disposable fixture
construction, loopback HTTP, disk work, test assertions and redirected log I/O;
it excludes parent output validation. This is a coarse candidate-only measurement,
not isolated API latency or native worker/render delivery. Raw stdout/stderr,
warm-up, samples and executable hash are retained under `components/provider-fixture`.
It isolates app data and temporary files under that directory and requires no
additional Rust compilation. The summary labels this boundary explicitly.

Frame intervals include event-loop scheduling, layout and paint submission;
focus/clear/menu intervals run from production handler dispatch to the next
qualifying native frame. They exclude physical input latency and display scanout.
Candidate-only scenarios cannot establish a before/after speedup; use the shared
comparisons to detect regressions. Correlated frame samples and 15-launch p95s
are descriptive evidence, not confidence intervals. Retain all runs and write
the final decision, confounders and limitations in `$benchRun/report.md`.

For remote-diff routing or refresh changes, generate a fresh cached remote fixture
with `remote::history::tests::prepare_native_fixture` after builds finish. Run
`benchmark-desktop.ps1 -Scenarios remote-diff,remote-pair` against it: these wait
for ghost metadata, use the normal graph selection/Diff handlers, and require a
nonempty file list without an error. They measure selection through selected-file
worker delivery, excluding physical input and scanout, and are candidate-only
for binaries without these scenarios. The ignored engine harness
`engine::tests::benchmark_remote_diff_refresh` measures parent and divergent-pair
diff recovery after clearing graph metadata (one warm-up and 15 cached samples).
It verifies the Git object remains absent and refs unchanged. Compare normal
startup with the preserved baseline; keep all generated samples under target/bench.

For Git mutation and recovery changes, also follow the operation-specific
[worker and native measurements](git-operations.md#performance-validation).
Compare the existing desktop mutation harness against a preserved baseline test
executable; branch/merge/rebase/history actions also need their own measurements.
When building archived source with the same Cargo target directory, Cargo can
reuse the wrong package artifact afterward. Preserve the baseline harness, then
run `cargo clean -p git-rustle --release` before rebuilding the candidate, and
verify that the expected new test names appear in its test listing.

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
to `benchmark.ps1` and `benchmark-remote-preview.ps1`. Hiding the native window
prevents a meaningful GPU frame measurement.
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
For Settings changes, compare `-Scenarios settings` against the preserved baseline.
Also run candidate-only `-Scenarios settings-tabs` to cycle Project/GitLab/GitHub
through the UI action handlers for 120 measured next-frame intervals. This includes
input synchronization and rendering, but excludes physical pointer/keyboard latency.
Each scenario gets one discarded launch and 15 measured launches. Scroll collects
121 next-frame intervals and discards the first, saving 120 raw intervals per
launch. This includes event-loop scheduling, layout and paint submission; it
excludes input-device latency and does not measure scanout or claim display FPS.
The other intervals are worker request to result delivery. State summaries include
maximum visible list-row counts to detect a zero-height viewport. They are not a
substitute for visual or mouse/clipboard verification.

For image-cache controls, run `scripts/benchmark-image-cache.ps1 -Candidate <exe>
-Image <small-PNG> -OutputDirectory <new-run-directory>`. It creates a disposable
repository and isolated app-data directory with 500 image files and 500 author
bindings. Candidate-only `settings-cache` measures size request through the first
ready frame; `settings-cache-clear` uses the real clear action and measures through
the empty-cache frame. Each has one warm-up plus 15 launches; the runner restores
the seed before each launch and verifies counts/bytes. Deletion requires a marker
in the isolated app-data directory. Never point this fixture at user app data.
These intervals include filesystem work, scheduling and frame submission, but
exclude physical input and scanout. Compare existing `settings`, `settings-tabs`,
`scroll`, `avatar-scroll`, cached-avatar delivery and startup against the baseline.
Check round photo edges and the size/clear controls in the native window too.

For footer panel changes, also run `-Scenarios status-panels` on a small disposable
repository. It cycles Details open/closed and History open/closed through the UI
handlers for 120 measured next-frame intervals. History inspection runs only when
opening its panel. The runner checks that the final Details panel is open. This
is candidate-only for binaries without the scenario; verify real pointer toggles,
selected button styling and dismissal separately in the native app.

For application updates, preserve the release executable before editing, then
compare native startup and `-Scenarios scroll,status-panels` against that baseline.
Run candidate-only `-Scenarios update-available` on a small disposable repository:
it shows a synthetic update and alternates its confirmation open/closed for 120
measured next-frame intervals per launch. Installation is disabled in this scenario.
The normal benchmark scenarios skip live update checks for reproducibility.
Verify the footer's absent/available states, pointer opening and Later separately.
After all builds finish, run the controlled loopback component measurement:

```powershell
$env:GIT_RUSTLE_UPDATE_BENCH_EXE = (Resolve-Path target/release/git-rustle.exe).Path
cargo test --release --locked update::tests::benchmark_update -- --ignored --exact --nocapture
```

Discard any invocation that compiles first. Save the repeated stdout under the
ignored run folder. The harness discards one warm-up and retains 15 samples of a
GitHub-format release check and a verified download/install of the full release
executable to a disposable path. These include loopback HTTP, hashing, disk I/O
and the crate's replacement path for a non-running destination. They exclude
public Internet latency, GPUI, restart, and Windows running-image cleanup. The
separate subprocess regression exercises replacement of a running Windows test
executable. Record limitations alongside startup and native interaction samples.

For check cadence and cache changes, also run
`cargo test --release --locked update::tests::benchmark_check -- --ignored --exact --nocapture`.
It retains one warm-up plus 15 samples each for metadata requests, startup checks
with recent saved state, and scheduled checks including persistence. All checks
now refresh the feed; saved results never suppress startup requests. It uses the
production check functions with a controlled clock and a loopback release server.
For actual network timing, explicitly run
`cargo test --release --locked update::tests::benchmark_live_check -- --ignored --exact --nocapture`.
This makes six unauthenticated, read-only GitHub release requests (one warm-up,
five measured). Save the raw outcomes as well as durations: a 404 or rate limit
measures that response, not successful release discovery. These measurements
exclude UI dispatch/paint and are separate from executable download/install.

For scrollbar or diff rendering changes, also run `-Scenarios files-scroll,diff-scroll,diff-pan`
against a fixture with hundreds of changed files and a long, wide modified text file.
These scenarios collect the same 120 frame intervals while moving the file list,
the diff vertically, or both diff text panes horizontally. They do not measure mouse
input latency. The pre-scrollbar-fix executable lacks these scenarios: report them
as candidate-only measurements unless both executables include the instrumentation.

For syntax-coloring changes, also follow the [source-code fixture and timing recipe](syntax-highlighting.md#performance-verification). Plain-text scrolling does not exercise token shaping.

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

For tab, context-menu or wrapping changes, `benchmark-desktop.ps1 -Baseline <exe>`
alternates baseline/candidate launches after one warm-up each. Use `scroll`,
`search-scroll` and `branches` on large history, and `files-scroll`, `diff-scroll`,
`diff-pan`, `diff` and `compare` on the wide-file fixture. The harness rejects a
zero-height graph viewport for graph scenarios. Candidate-only `wrap-scroll`
records initial wrap-layout CPU time separately from its 120 cached scroll frames;
`tabs` alternates Graph/Diff and `graph-resize` alternates the graph column width.
`compare-menu` opens a two-commit context menu for visual inspection and asserts
that its Checkout action cannot open a checkout dialog. It does not exercise
physical Ctrl+click. Keep native clipboard and pointer checks separate.

Retain all raw data, executable hashes, limitations and failed/rejected runs.
For undo history, preserve both release application and test executables first.
After final release validation/builds, prepare a new disposable fixture with
`GIT_RUSTLE_UNDO_FIXTURE=<new-path>` and run the ignored test
`git::operations::tests::prepare_undo_fixture`. It records 100 real branch actions
and restores position 80. Run `scripts/benchmark-undo.ps1 -Fixture <path>
-Baseline <exe> -OutputDirectory <new-directory>`. The suite alternates 15
baseline/candidate openings after one warm-up each, then measures candidate-only
scrolling (120 intervals per launch) and clearing (worker delivery and empty-panel
frame). Each clear starts from a restored fixture journal; the runner checks
durable clearing plus unchanged HEAD, refs, staging, files and config. These
scroll intervals include handler CPU, scheduling, layout and paint submission;
they exclude physical input and scanout. The older baseline lacks scroll/clear
instrumentation. Check pointer controls, current/redo styles, confirmation and
cancellation separately. Compare large-clone startup and the existing ignored
`engine::tests::benchmark_desktop_mutations` on both preserved test executables.

For the interactive rebase editor, run `scripts/benchmark-rebase.ps1 -Baseline <exe>
-OutputDirectory <new-directory>` after release validation/builds. It creates a
disposable linear 1,001-commit repository, alternates baseline/candidate openings
of the same three-commit plan, then measures candidate-only scrolling and editing
of 1,000 rows (one warm-up and 15 launches; 120 measured next-frame intervals per
interaction launch). Editing cycles all six actions, moves a commit in both
directions and updates a reword message through the UI handlers. The runner checks
visible rows, validation state, unchanged HEAD/worktree and absence of executed
operations. Intervals include event-loop scheduling, layout and paint submission;
they exclude action-handler CPU time, physical input and display scanout. To
measure the model work too, compile `scripts/benchmark-rebase-model.rs` with
`rustc -O --edition=2024 -C lto=fat -C codegen-units=1 -C panic=abort -o
target/bench/<run>/model-bench.exe`, then run it after builds finish and save stdout
as `model-samples.json`. It imports the production draft module, using only the
Plan text field, and records a discarded warm-up plus 15 samples of loading 1,000
entries and 200 action changes, moves, message validations and serializations per
sample. These component times exclude GPUI, Git and input delivery; do not add
their percentiles to native frame percentiles. The baseline lacks the structured
interaction scenarios. Verify actual combobox, keyboard, move and message-editing
behavior separately in the native app; run startup comparisons on the large clone.

For native checkout-dialog styling, run `benchmark-desktop.ps1 -Scenarios checkout-dialog`
on the disposable three-commit checkout fixture. It selects the second graph row,
opens the confirmation without checking out, and records 120 modal redraw intervals.
The runner requires an open dialog in the final state and rejects any checkout request.
Both comparison executables must include this scenario; preserve an instrumented
baseline before changing the design. This measures redraw scheduling/layout/paint,
not physical input latency; verify pointer interactions separately in the fixture.

For framework migrations, report the startup regression explicitly even when
scrolling remains fast; moving to GPU rendering does not imply faster startup.

## 1. Preserve a trustworthy baseline

### Provider composition and photos

For empty-description regressions, also run candidate-only
`benchmark-desktop.ps1 -Scenarios request-open,request-clear` on a disposable local
repository with one commit and no provider remote. `request-open` uses the actual
NewRequest action from the toolbar/context menu and leaves the description empty;
it measures action-to-rendered-frame time. `request-clear` alternates Markdown text
and an empty description for 120 measured frame callbacks, ending empty. Both
require an empty editor in the final snapshot and perform no submission. Compare
the existing `request-compose,request-edit` scenarios with the preserved baseline
for construction/editing regressions. A prefilled composer alone cannot validate
empty-placeholder rendering: native pointer opening from both entry points and
typing/clearing the description must also work.

After preserving the baseline and finishing release validation/builds, compare
startup and `benchmark-desktop.ps1 -Scenarios scroll,branches -Baseline <exe>` on
the large history. Run candidate-only
`benchmark-desktop.ps1 -Scenarios request-compose,request-edit,request-tabs` on a
small disposable repository. These use 10,000 synthetic project members, the real
composer input states, and the normal tab handlers without submitting requests.
`request-compose` records composer construction/member-index installation to ready
and its next frame; edit alternates Markdown Write/Preview and changes the title;
tabs alternates Graph/composer. Each interaction launch retains 120 next-frame
intervals after discarding the first. The runner verifies the retained composer,
member count and idle submission state. This is candidate-only UI instrumentation,
excluding provider latency, physical typing and scanout. Verify actual member
search, keyboard selection, chip removal, tab retention and preview with the native
UI separately. The autocomplete component measurement includes query filtering CPU:

```powershell
cargo test --release desktop::requests::tests::benchmark_member_search -- --ignored --exact --nocapture
cargo test --release remote::requests::tests::benchmark_roster -- --ignored --exact --nocapture
cargo test --release remote::requests::tests::benchmark_author_cache -- --ignored --exact --nocapture
```

Discard any invocation that compiled first, then save repeated harness stdout to
the ignored run folder. Each harness discards one warm-up and records 15 samples.
Member search uses 10,000 records and 600 queries per sample; the roster harness
compares a 100-member loopback project/member load with fresh disk-cache reuse
after its HTTP server has stopped. Neither establishes real provider latency.
The author cache harness measures a loopback commit identity lookup with seeded
image bytes, then both identity and image reuse with the API server offline. It
excludes actual image download/decoding. For image paint/scrolling, set
`GIT_RUSTLE_AVATAR_FIXTURE` to a small PNG/JPEG/WebP and run candidate-only
`benchmark-desktop.ps1 -Scenarios avatar-scroll` on the large history. This uses
one shared cached image for all visible rows and records 120 next-frame intervals
per launch; it does not characterize mixed-image cache misses or network latency.
Normal provider fixture tests cover creation payloads for both providers, GitHub
partial success, pagination, token cache partitioning, failed rosters and redirects.
Record authenticated live checks separately using the ignored
`remote::requests::tests::live_github_readonly` test, with `GITHUB_TOKEN`,
`GIT_RUSTLE_LIVE_PATH` and optionally `GIT_RUSTLE_LIVE_PROJECT=owner/repository` in
the child process environment. Never log the token. It only reads project/members,
the latest commit and its image; request creation remains a local fixture test.
Keep native cached remote-preview comparison in the run as a regression check.

For shared binary-photo cache changes, preserve both the baseline application and
test executables. Compare the existing `benchmark_author_cache` harness in each
preserved test executable after builds finish: it records 15 samples after one
warm-up, with 100 warm reads per sample. The candidate seeds equivalent 4 KiB image
bytes directly in its binary cache; the old harness seeds a JSON byte array. These
measure cached identity/image I/O and parsing, excluding image decoding and real
network downloads. Also run candidate-only
`remote::requests::tests::benchmark_shared_author_cache` to resolve different commit
IDs through the same persisted email binding with the API unavailable. It reports
binary versus JSON storage sizes for the same payload.

Set `GIT_RUSTLE_AUTHOR_REPO` to the large read-only clone and run the ignored
`desktop::requests::tests::benchmark_local_author_reuse` harness. It reads 40 local
author headers per sample and measures shared-image lookup separately. It excludes
worker scheduling and GPU paint. Compare native startup, `scroll` and `avatar-scroll`
with the baseline; `avatar-scroll` uses a preloaded shared photo. Then run candidate-only
`avatar-reuse-scroll` with `GIT_RUSTLE_AVATAR_FIXTURE` set to that photo. This scenario
uses the real background local-author worker and profile lookup while scrolling,
seeding positive profiles with the fixture image after each lookup and issuing no
photo API requests. Logs record worker dispatch-to-delivery milliseconds, snapshots
require visible reused profiles, and frame JSON retains 120 intervals per launch.
One photo is shared by all seeded identities: this measures reuse/scheduling/rendering,
not mixed-image decoding, cold provider responses or physical input latency.
`avatar-reuse-scroll` keeps the process alive until at least 2.5 seconds after
the first frame so the final viewport's background author lookup can settle
before its state assertion. Its 120 measured scroll intervals end before this
settling period; the assertion still requires visible reused profiles.

For cached-avatar delivery, preserve an application baseline containing the
`avatar-cache` instrumentation before changing production image handling. On the
large clone with a fresh persisted binding and image for its first graph author,
run `benchmark-desktop.ps1 -Scenarios avatar-cache -Baseline <exe> -Samples 15`.
The scenario clears session avatar state, scrolls to the top, and measures from
that request to the next-frame callback after GPUI has decoded the first visible
photo. It uses the real local-author worker and existing disk cache; other authors
may have cache misses. It includes worker scheduling, cache I/O, decoding and
frame submission, excluding physical input and display scanout. Normal network
requests remain enabled, so record cache/network conditions and retain the logs.
Run candidate-only `avatar-cache-busy` too: it holds the photo-network worker busy
and requires the same cached photo to become renderable independently. The old
implementation cannot complete this second scenario. Compare `scroll`,
`avatar-scroll` and `avatar-reuse-scroll` against the preserved baseline separately;
do not infer scrolling performance from the single-photo delivery interval.

For landing-page changes, compare native repository startup with the baseline,
then run `scripts/benchmark-landing.ps1 -Candidate <exe> -Repository <repo>
-OutputDirectory <new-directory>`. The candidate-only suite discards one warm-up
and retains 15 launches each for landing with 20 entries, opening the first recent
repository, and forgetting it. It uses isolated history, includes stale paths,
and checks persisted MRU/removal and final model state. Landing timing ends at
the next-frame callback; open timing starts at the shared repository-open handler
and ends at its first history frame; forget timing includes storage and the next
redraw callback. These exclude physical input and folder-picker navigation. Check
the native picker, cancellation and row clicks separately on an unlocked desktop.
Use `GIT_RUSTLE_RECENTS_FILE` for history isolation in other benchmark suites too.

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

Keep binaries, fixtures, logs, screenshots, raw samples and summaries under the
unique `target/bench` run folder. Write `report.md` in the same folder with:

1. Problem/behavior changed and which performance paths could regress.
2. Date, environment, repository identity/counts, baseline and candidate hashes,
   source state, and executable sizes.
3. Exact commands, warm-ups, sample count/order, cache and network conditions.
4. Timing boundaries and exclusions; before/after table with median/p95/deltas.
5. Findings and corrective iterations, including rejected slow implementations.
6. Test/visual checks, limitations, and links to raw data.

Include the report location and key findings when delivering the change. These
artifacts are local and may disappear when `target` is cleaned; copy them outside
the checkout if longer retention is needed. Do not commit generated results or
link repository documentation to local run artifacts. Historical results remain
available in Git history. Documentation-only edits need link/command review, not
new performance runs.

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
