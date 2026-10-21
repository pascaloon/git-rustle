# Benchmark workflow for agents

Git Rustle prioritizes speed. Use this workflow to detect regressions before
delivering notable changes. Commands below run in PowerShell from the repository
root. Keep benchmark instructions, scripts and harness code in version control;
write generated results to unique ignored `target/bench/<run>` directories.
Additional scenario-specific scripts and their setup requirements are listed in
[the benchmark helper guide](benchmarks/README.md).

## Default unattended validation

Follow [the repository agent instructions](../AGENTS.md): use automated actions,
disposable fixtures and paint assertions without Computer Use or interaction with
the user's desktop. Native GPUI launches must use
[`benchmark-native-process.ps1`](../scripts/benchmark-native-process.ps1), which
keeps rendering enabled on a private, non-current Windows desktop without
switching the input desktop. Inspect scenario scripts before running them and
adapt any legacy visible-window launch to this helper first. If isolation fails,
report the gap and continue independent checks; do not fall back to an interactive
window. This default also applies to reproduction recipes recorded in older
reports. Compare baseline and candidate under the same desktop conditions and
disclose that private-desktop paints exclude physical input and display scanout.

For visual changes, also follow [the app screenshot workflow](app-screenshots.md).
`scripts/screenshot-app.ps1` captures only the isolated app window for image
review. Run it separately from timing suites: screenshot capture synchronizes
rendering and its durations are not performance samples.

### Stable graph order and selected paths

The next graph feature bundle adds `scripts/benchmark-graph-features.ps1`. Supply
the final candidate and a fresh ignored output directory; its default matrix
retains one warm-up plus 15 samples for `graph-multiselect`, `multi-details`,
`dense-labels`, `checkout-simple`, and `diff-base`. It builds clean disposable
main/topic fixtures, invokes real handlers, awaits loaded workers and relevant
GPUI paints, checks column bounds and index/worktree/ref postconditions, and
launches through the private desktop helper. `-Screenshots` runs a separate
single capture per view; inspect those PNGs rather than treating pixel checks as
visual review. The candidate-only intervals measure request through verified
frame and include worker dispatch and final frame scheduling. They are not a
comparison against the former confirmation workflow.

Also run `desktop::hover::tests::benchmark_multi_selected_paths` (100k records,
80-row viewport, 1/4/16 anchors), `checkout::tests::benchmark_branch_choices`
(1k/10k logical branches), and `git::branch_base::tests::benchmark_branch_base`.
The latter uses `GIT_RUSTLE_BENCH_REPO` and optional full hexadecimal
`GIT_RUSTLE_BENCH_BASE_COMMIT`, recording target preparation, actual merge-base
lookup and resulting tree diff separately. Measure startup and normal/searched
scrolling against the preserved application baseline as shared controls.

Compare warm/uncached startup and normal/searched scrolling against the preserved
application baseline. Run `benchmark-desktop.ps1 -Scenarios graph-hover
-RequireSelectedPath` with the usual repository, binary, sample and output
arguments. The candidate must verify the selected path after a native paint while
hover repeatedly enters and leaves, retaining the 120-frame measurements.
The baseline runs its existing hover behavior without the new assertion.
`desktop::hover::tests::benchmark_selected_path` records one warm-up plus 15
component samples of selection, hover, scrolling, repeat-frame cache hits and
flag unions on 100,000 records with 80 visible rows. Each reported interval is the
mean per paired-frame iteration over 1,000 iterations, excluding native painting.
`selectionMs` separately measures building the complete selected track once;
later scroll frames read only visible rows from that cached track, including
when the selected commit is outside the viewport. Streaming history growth,
changed reference fingerprints and partial-search geometry invalidate the trace.
`git::graph_order_tests::benchmark_graph_walk` uses `GIT_RUSTLE_BENCH_REPO` and
separately reports reference snapshot, first batch and full walk intervals.
`git::graph_order_tests::benchmark_equal_timestamp_frontier` creates a disposable
10,000-commit same-second chain and measures its first batch and complete walk.
Equal-time frontiers are discovered before emitting any row so referenced
ancestors follow their descendants; this pathological case deliberately measures
the startup cost of expanding an unusually large timestamp frontier.
The latter two components establish candidate costs; preserved baseline native
startup and scrolling remain the shared comparisons. The history-cache version
changes, so distinguish fresh candidate cache creation from warmed reopening.
Detached HEAD history unreachable from references is appended after reference
history to avoid a second ancestry pass; reference-reachable checkout preserves
existing rows even with equal timestamps or clock skew. This is a display-order
policy, not a new strict topological-order guarantee.


## GPUI desktop (current implementation)

### Conflict inspection and live refresh

After preserving exact application and test baselines, use the focused runner
with final optimized binaries:

```powershell
.\scripts\benchmark-conflicts.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -BaselineHarness $baselineTestExe `
  -Candidate $candidateExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

Shared controls cover fresh warm/uncached startup, graph scroll/search-scroll and
the existing `git::operations::tests::benchmark_stashes` worker. New native cases
bundle merge/rebase/stash conflict opening, both commit columns and actual details,
whole-file Incoming confirmation, an unsaved draft preserved across an external
edit, rejected stale Save, explicit Reload and built-in editor save, then an
external file write/index stage observed by the watcher and enabled Continue. A separate
merge case verifies the painted Abort warning and real Abort. Backend correctness
tests cover operation-specific abort semantics. Each launch creates a clean
three-file divergent fixture; real operation actions establish the conflict and
recovery journal before the measured inspection intervals. The exact marker is
`.git/git-rustle-conflicts-fixture`, containing
`disposable conflict benchmark fixture`. Local hooks/signing/editors are disabled;
no real external editor or public remote is used.

One warm-up plus 15 retained launches per group gives 192 native executions and
three component invocations for the default matrix. The candidate
`git::operations::tests::benchmark_conflict_refresh` separately measures initial
metadata, **20** unchanged worker probes, changed-file refresh and conflict preview.
These include real bounded repository I/O; `idle_20_ms` is the whole 20-probe batch,
not one poll or an artificial wait. Native marks retain each operation through
verified paint; watcher latency includes its normal scheduling cadence. New
interactions have candidate-only costs, not an invalid comparison against missing
baseline behavior. Shared frame intervals and per-launch means remain separate.
The legacy conflict runner opens a window on the current desktop. Adapt its
native launch to the private-desktop helper before running it unattended; GPUI
paint assertions require rendering, not presentation on the user's desktop.
Background helpers stay hidden.
Rejected-save disk verification runs outside its named request-to-paint and Reload
intervals. The combined request-to-final-frame interval includes all workflow
checks and watcher waits; separate medians must not be added together.

The runner isolates app-data/temp, rejects competing builds, and retains raw
samples plus source/binary/shared-fixture integrity. The companion
`summarize-conflicts.ps1` validates groups and classifies every metric: strict joint
5%/1ms worker/startup gates and 5%/0.10ms frame gates at median and p95. Preserve all
crossings and bounded repeats; keep previous unresolved findings outside this
scope explicit. Local fixtures and native paint callbacks do not establish all
repository scales, employer-network access, physical pointer input or display
scanout. Fresh measurements are required after any implementation change.

### Saved external-tool list

For the persistent tool list, one-time background discovery and Diff picker, use
the focused runner after preserving application and test baselines from the exact
pre-change source and completing functional checks/final optimized builds:

```powershell
.\scripts\benchmark-tools-list.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -BaselineHarness $baselineTestExe `
  -Candidate $candidateExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

The shared controls are warm/uncached startup, ordinary selected-file Diff,
General tab frames and controlled manual-update/cache actions. The candidate
native external-tools case verifies real Configure/list/editor/picker actions;
four actual fields must paint at usable width, with name/executable heights of
32 pixels and argument heights of 92 pixels. The shared worker harness measures
discovery/preparation and real controlled process wait/merge output. The new
`external_tools::tests::benchmark_tool_list` isolates first scan, saved reload,
saving, legacy migration and stable-ID selection. New features establish absolute
cost; they do not have a baseline speedup claim. This bounded suite intentionally
avoids unrelated branch/search/stash measurements.

Each native group has one discarded warm-up and 15 retained launches, alternating
shared binary order. Each component mode retains 15 samples after its own warm-up;
executables run sequentially, with no competing builds. The runner isolates
LOCALAPPDATA and TEMP, records raw logs/source/binary/fixture provenance and checks
integrity. First-scan native launches receive fresh app-data and two fake install
roots, guarded by `.git-rustle-tools-list-fixture` with exact contents
`disposable external tools list fixture`. The opt-in discovery-root environment
override is ignored without that marker. Fake applications are never launched;
configuration and scan-completed persistence are verified even when discovery is
empty. Fixture preparation and postcondition checks are outside measured clocks.

Preflight rejects compiler/linker processes and Cargo builds/tests. An existing
`cargo run` is allowed only after Windows process metadata proves its parent
chain is waiting on a live Git Rustle executable under this workspace's target
directory. Missing permissions or ambiguous ancestry reject the run. PID/start
identities are checked again before reuse; the permitted background application
and launcher ancestry are recorded without command arguments. Keep the existing
user application open and disclose that it can still consume CPU/GPU/I/O; this
exception does not establish zero background load.

The companion summarizer retains pooled frame timings and per-launch means,
reports separate list-action/worker intervals, and records every shared joint
median/p95 increase above 5% and 1 ms for workers/startup, or above 5% and 0.10 ms
for frames/in-memory CPU. Retain all crossings and any bounded diagnostic repeats;
do not average away failures. Scan/loopback fixtures do not measure the user's
installed editor startup or interactive editing latency, OS credential access,
physical pointer/display scanout or employer-network access. Store the report and
raw samples under ignored `target/bench/<run>`; use new measurements for each
implementation and preserve baseline executables before edits.

For the graph/branch/stash/tools/reference-storage bundle, use the sequential
[`benchmark-todos.ps1`](../scripts/benchmark-todos.ps1) runner with preserved
application **and test harness** executables from the exact initial source:

```powershell
.\scripts\benchmark-todos.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -BaselineHarness $baselineTestExe `
  -Candidate $candidateExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

Finish product changes, functional checks, Clippy and final builds before this
run. Preserve both binaries outside Cargo's build output and record fingerprints;
when sharing a target directory between archived/current source trees, force a
package-only clean before building each source tree to prevent manifest-path
artifact reuse. The runner builds nothing and rejects competing compilers. A
prepared large local-origin mirror can be reused, but collect new samples for the
new implementation. Generated output must stay under ignored `target/bench/`.

| Area | Measurements |
| --- | --- |
| Startup and existing graph behavior | Warm/uncached startup with post-frame public update checks suppressed by `startup-local`; normal/searched scrolling, branch activity, search, diff delivery and existing settings tabs against the preserved baseline. |
| General settings layout and controls | Shared `general-tabs` renders General in 120-frame tab cycles; `general-settings` compares controlled loopback current/available/error manual checks and isolated eight-image cache clearing through verified paint, plus the bundled request-to-frame interval. Both binaries receive fresh cache fixtures per launch; no public update HTTP or installation occurs. |
| Pending/HEAD/Diff navigation and graph layout | Candidate `todo-graph` verifies the bundled real actions and final frame, with staged, unstaged and untracked changes and another branch newer than HEAD. |
| Branch sorting and hover | Candidate `branch-sort` and `graph-hover` each retain 120 native frame intervals per launch; separate component sorting at 1,000/10,000 entries and viewport-bounded hover CPU. |
| Stash panel/apply/delete | A fresh clean three-stash fixture per native launch; apply retains all entries and produces pending changes, delete removes only the selected entry. Shared commit/checkpoint workers measure the affected journal path; candidate stash workers also measure undo/redo and conflict/abort. |
| External tools | Candidate General controls render without launching installed applications. Worker samples use an isolated controlled executable, byte-exact temporary inputs, real process waiting and merge output; they exclude actual installed GUI editing latency. |
| Reference storage and case-distinct checkout | Native explicit conversion operates only on a fresh marked repository; backend components keep files/reftable snapshot/head costs separate, and Case/case checkout samples stay separate. No automatic conversion occurs while reading a repository. |
| Cached icons, request badges and MR targets | Shared provider cache/lookup/icon and graph search controls; candidate fresh-client cached-icon reopening, one 10k-association search projection, and bounded parent-target inference. Inference reports both parent-model CPU and actual repository/ref/object access. |

The runner creates isolated `LOCALAPPDATA`, `TEMP` and `TMP` directories with
markers. Process-only Git configuration trusts only the workspace/read-only
fixture and the marked disposable roots; it restores all inherited environment
values afterward. Native mutations require the exact marker
`.git/git-rustle-todos-fixture` containing `disposable todos benchmark fixture`.
`benchmark-desktop.ps1 -DisposableFixtureRoot` refuses an unmarked fixture root,
creates a new repository for every warm-up/measured launch, retains it for
inspection, and independently checks HEAD/branches and the expected stash or
index/worktree outcome. The marker is inside `.git`, so it does not create a
pending untracked file. Fixture preparation and postcondition checks are outside
the timed native interval. The large mirror is never used for native mutations.

Every native group has one warm-up and 15 retained launches. Shared launch order
alternates; component harnesses contain their own warm-up/15 retained samples and
run as separate binary blocks, reversing order between shared harnesses. Keep
`entries`, `backend`, `distribution`, `branch`, `dirty` and search `operation`
groups separate. The parser uses case-sensitive grouping for Case/case, rejects
missing/duplicate component samples and records stage completeness. Frames within
a launch are correlated: report both pooled values and 15 launch means. Native
bundled action-to-frame durations cover the whole scenario, while component
workers isolate the affected operation. Neither establishes physical pointer or
display scanout latency.

`branch-sort` requires at least two branches with loaded activity; `graph-hover`
requires history longer than the visible viewport. Both use the prepared large
mirror. Their 120 action-to-paint intervals include the actual handler, with
120 separate handler-CPU values and paint-bound observations per launch; assertion
work occurs outside each recorded frame clock. Retain pooled handler values and
15 handler launch means separately rather than adding CPU twice to frame time.

Activity-recency weighting remains a **test-only prototype**. Its shared-tip,
neighboring-tip and spread-tip distributions compare old counts with weighting
using the same bounded worker body and alternating internal order. Inspect
`prototype-recency-summary.json`; those paired controls are not production
baseline comparisons and are excluded from the main comparison rows. A weighted
ranking must not be integrated unless the user's no-extra-cost constraint is
satisfied. Preserve the current ranking if the measurements do not justify it.

Cached-image reopening uses fixture credentials and disk data, excluding OS
credential discovery and native decode/paint; a controlled streaming test checks
that the cached image arrives while request-list HTTP is blocked. Provider
metadata changes refresh active badge queries without HTTP on typing. Parent
inference is bounded and uses available local refs/objects; ambiguous siblings,
missing objects or budget limits fall back to the provider's actual default.
Stash samples cover one entry in worker fixtures and three native entries, not
scaling across thousands of existing stashes. Loopback and local-origin fixtures
do not establish employer-network access or production credential correctness.

Use `-Stages Startup,History,Actions,Components` to resume only unfinished stages;
existing raw stage directories are never overwritten. `-Samples` controls native
launches only; component groups always retain 15 samples. Run
[`summarize-todos.ps1`](../scripts/summarize-todos.ps1) after measurements to rebuild
tables without rerunning executables. Retain raw logs, stage/provenance/integrity
JSON, source fingerprints, failed smoke runs and `report.md`. Report absolute and
percentage median/p95 deltas, all threshold crossings and limitations; finite
measurements cannot guarantee zero regression. Do not edit measured source or
scripts after final builds or during the run.

For branch-author search, active-filter/HEAD styling, GitLab icons and General
settings, use the focused sequential runner:

```powershell
.\scripts\benchmark-branches-settings.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -BaselineHarness $baselineTestExe `
  -Candidate $candidateExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

Preserve both baseline binaries from the exact initial source, including existing
uncommitted work. When building an archived baseline in a shared Cargo target,
force a package-only clean before each source tree's build: Cargo can reuse a
same-package test artifact across manifest paths. Preserve application and test
executables outside that target before cleaning, and verify source fingerprints.
Build the final candidate and finish checks before measurements.

This bundle compares startup, graph/search scrolling, branch activity and existing
settings tabs. A combined native filter scenario checks real focus/blur, clearing,
author-only branch matches and HEAD painting; General scenarios cover four tabs,
isolated cache reset and forced loopback update checks (current/available/error),
including duplicate-check suppression and zero downloads. Provider menu/scroll
scenarios retain dense badge and context-menu coverage. Shared component harnesses
cover 1,000 branch tips, request-cache reading, visible-label lookup and update
checks; new components measure author filtering at 1,000/10,000 entries and
GitLab icon normalization, authenticated loopback download and cache reuse.
The branch component's drawing measurement uses the legacy adapter; native
scrolling is measured separately. Loopback tests cannot establish access to an
employer's GitLab network or validate its configured credentials.

Each native group and component uses one warm-up and 15 retained samples. Native
baseline/candidate launch order alternates; component binaries run separately,
with order reversed between shared harnesses. Report median/p95 and absolute as
well as percentage differences, and inspect per-launch frame means. Keep raw
samples, initial/final source fingerprints, failed smoke logs and a Markdown
report in the ignored run folder. Prior measurements are not substitutes. Stages
can be resumed with `-Stages` without overwriting completed results, and
`summarize-branches-settings.ps1` rebuilds tables without rerunning measurements.

For tag creation and the fixed graph-count slot, use the focused sequential
[`benchmark-tags-toolbar.ps1`](../scripts/benchmark-tags-toolbar.ps1) runner:

```powershell
.\scripts\benchmark-tags-toolbar.ps1 -Repository $benchRepo `
  -Baseline $baselineExe -BaselineHarness $baselineTestExe `
  -Candidate $candidateExe -CandidateHarness $candidateTestExe -Samples 15 `
  -OutputDirectory (Join-Path $benchRun 'suite')
```

Preserve both baseline executables before implementation, validate native hooks
with a fast-profile functional smoke first, then finish final release tests,
Clippy and builds before timing. The runner executes already-built harnesses
directly and rejects competing compilers. A previously prepared large local-origin
mirror may be reused to avoid fixture setup; collect new measurements for the new
implementation. Record its identity/refs, both application and harness hashes,
and any known harness-source differences. An earlier compatible test executable
is valid only when the shared measured production functions are unchanged; explain
that limitation instead of assigning it the current source revision.

Shared native measurements are startup (warm/uncached), normal/searched graph
scrolling and search delivery. Candidate-only `toolbar-counts` verifies unchanged
search/count bounds across six count states and four width/count-size contexts,
with 120 measured native frame intervals and 24 real state-transition CPU samples
per launch. Query-result and resize waits precede sampling; handler CPU is reported
separately from event-loop/layout/paint intervals. Inspect 15 per-launch means as
well as correlated pooled values. Bounds assertions are layout evidence, not
physical pointer or scanout latency.

Native `tag-dialog` and `tag-create` use a fresh four-commit repository for every
launch. Opening times action-to-frame without submitting. Creation uses the real
dialog/inputs/submit path and times submission through a refreshed graph tag label.
The runner independently verifies the annotated `bench-tag` at the frozen selected
commit (`HEAD~1`), its message, unchanged HEAD/branches and a clean worktree.
All mutations remain in those disposable fixtures, never the reused large mirror.
The candidate `git::operations::tests::benchmark_tags` harness measures lightweight
and annotated creation plus undo/redo (one warm-up and 15 values for each type),
asserting preservation of HEAD, staging, working and untracked contents.

The bounded shared worker comparisons are
`engine::tests::benchmark_desktop_mutations` and
`git::operations::tests::benchmark_large_worktree_operations`. These cover normal
commit/checkout delivery and the checkpoint create/undo/redo path with 1,001 files,
clean and dirty. Each harness has its own warm-up and 15 samples per measured
mode. The two binaries run as separate harness blocks; order reverses between
the shared harnesses, rather than interleaving internal samples. Their worker
and Git-process costs exclude native painting; engine polling has approximately
2 ms resolution. Broader merge/rebase/push suites are unnecessary for this bundle
unless implementation expands into those paths.

Use `-Stages Startup,History,Toolbar,Tags,Workers` to select unfinished stages;
existing stage results are retained. `-Samples` controls native launches only;
the component harnesses retain their fixed 15 samples. Save raw output, failures,
source/fixture integrity checks, acceptance criteria and the final report under
the new ignored run folder. `summarize-tags-toolbar.ps1` regenerates tables without
running measurements. Candidate-only features cannot establish a speedup against
the baseline; fast-profile smoke timings and previous-run samples are not final
release performance evidence.

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

`-NativeCandidate` selects native GPUI measurement; the baseline console stays
hidden. For comparisons between two GPUI builds, also pass `-NativeBaseline`
to `benchmark.ps1` and `benchmark-remote-preview.ps1`. Ensure both native launches
use the private-desktop helper, adapting legacy runners before use. Keep rendering
enabled there: hiding or minimizing the native window can suppress meaningful
GPU frame measurements. Do not mix current-desktop and private-desktop samples
in a baseline/candidate comparison.
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

### Stash creation and external stash changes

After preserving exact application/test baselines and completing final builds, run
`scripts/benchmark-stash-workflows.ps1` with `-Repository`, `-Baseline`,
`-BaselineHarness`, `-Candidate`, `-CandidateHarness`, and a new
`-OutputDirectory target/bench/<run>/stash` (15 retained native samples by default).
The runner preserves raw samples, binary/source hashes, fixture before/after
states and a Markdown report. Shared native scenarios cover panel listing,
apply-with-retention and deletion. Candidate-only `stash-create` uses the actual
optional-name form and Save action, verifies the named entry, then observes an
external unnamed push and external drop through the open panel and final paint.
Each launch uses a fresh marked repository; the final state has four stashes and
no pending changes. External mutation scheduling and the ordinary two-second
status cadence are included in their separate intervals. These are local fixture
measurements, not physical pointer or scanout measurements.

The existing stash worker comparison measures apply/delete/undo/redo/conflict;
`git::operations::tests::benchmark_stash_create` adds creation and listing.
Also compare the existing `git::worktree::tests::benchmark_pending` worker and
startup against the preserved baseline: external stash detection reuses the
existing status worker with four bounded metadata probes. No stash-list command
runs on unchanged status ticks. Treat candidate-only intervals as absolute costs;
report shared joint median/p95 regressions above 5% and 1 ms, preserving all raw
crossings and limitations.

### Graph selection, refresh feedback, stash and discard bundle

Use `scripts/benchmark-improvements2.ps1` after final checks/builds, passing
`-Repository`, the exact preserved `-Baseline` and `-BaselineHarness`, final
`-Candidate` and `-CandidateHarness`, and a new ignored
`-OutputDirectory target/bench/<run>/suite`. Default `-Samples 15` retains fifteen
native launches after one warmup per group. Stages are `Startup`, `History`,
`Actions`, and `Components`; resume with only unfinished stages. Existing stage
directories are retained and rejected if selected again.

Startup gives each binary its own cache root because the candidate's topological
history layout uses cache format v3. Shared history controls cover ordinary and
searched scrolling, selected-path/hover painting and footer panels; the candidate
also paints refresh feedback while preserving the project image. Actions invoke
the marked stash and discard workflows described above. Components compare the
existing pending-status and hover workers, then measure selected-path caching,
real reference-history walking, and single-file discard. Stash workers run once
inside the stash workflow wrapper. Component harnesses retain their own fifteen
samples regardless of the native `-Samples` setting.

Native startup, history and action launches use
`scripts/benchmark-native-process.ps1` to create a separate Windows desktop per
process. Windows paint normally there, but the helper never switches the user's
input desktop. It redirects stderr and retains the isolated desktop until exit;
timeouts terminate the process tree before releasing it. These measurements cover
GPUI paint callbacks on a non-current desktop, excluding interactive desktop
composition, physical input and display scanout.

The companion `summarize-improvements2.ps1` rebuilds `report.md`, `summary.json`,
`gates-metrics.json`, `gates-crossings.json`, and `stage-completeness.json` from raw
outputs without rerunning applications. It preserves dimension groups and checks
sample completeness. Strict joint gates are 5% plus 1 ms for worker/startup/action
delivery, and 5% plus 0.10 ms for frames, launch means and in-memory CPU.
Keep all crossings and diagnostic repeats visible. The native figures include
real action delivery and verified GPUI paint, excluding physical pointer/display
scanout; local disposable fixtures do not establish all graph/worktree scales or
employer-network behavior.

## October feature batch: hierarchy, cleanup, pipelines and diff search

Build final application and test executables before timing; never benchmark while
Cargo, rustc or a linker is running. Preserve both baseline executables first.
Use fresh subdirectories beneath the same ignored run directory for:

- `benchmark-graph-features.ps1`: real checkout, merge-base diff, multi-selection,
  expandable details and dense badges. Run once with `-Samples 15` and separately
  with `-Screenshots`.
- `benchmark-branch-cleanup.ps1`: hierarchy and paired branch rows, cleanup review
  and explicit leased deletion against a disposable local bare remote. Supply
  candidate/baseline test harnesses and a read-only repository for component
  comparisons. Use `-Stages Native,Components,Screenshots`.
- `benchmark-ui-features.ps1`: filename/content search, rapid query replacement,
  selected-result navigation, Escape through real GPUI key dispatch, modal
  precedence and General links. Screenshot metadata verifies the actual native
  project title. Use `-Samples 15`, then a separate `-Screenshots` run.
- [Provider/tool measurements](benchmarks/provider-tools.md): remote-only and
  deduplicated request badges, five pipeline states and actual executable icons
  in settings and the diff toolbar. These fixtures never launch external tools.
- Existing `benchmark-stash-workflows.ps1`: stash open/create/apply/delete and
  preservation of unrelated work; candidate now uses a full tab, so native
  sidebar-versus-tab frame costs are not identical visual workloads.

Diff search's ignored `diff::search::tests::benchmark_worker_cold_hot` records one
warmup plus 15 actual cold I/O/index and warm query samples on 32 changed files.
`benchmark_search` isolates a cached 100,000-line scan. The native search fixture
uses 16 changed 500-line files; input-to-completion intervals include the 65 ms
input debounce and worker notification/frame scheduling. Its scheduler pauses
while queries run so artificial frame polling cannot hide missed wakeups. An
8 MiB per-side read limit, 64 MiB retained text index, 1,000-result limit and
cancellation bound the work. Historical object sizes are checked before loading.
Report retained-cache bounds separately from temporary decoding allocations.

Run preserved baseline/candidate startup and graph scroll/search-scroll controls
on the same larger repository using `benchmark.ps1` and `benchmark-desktop.ps1`.
Measure `diff-scroll` as well as the new query operations. Keep cold-cache setup,
warmups and capture-only launches distinct. Record raw samples, medians/p95,
fixture scale, binary hashes and any regressions in the run's Markdown report.
New workflows have candidate-only absolute timings; do not invent a baseline.

Cleanup's `benchmark_cleanup_remote` also measures real leased remote deletion
and the local ref transaction with configured and nonmatching fetch mappings.
It records whether Git removed the tracking ref after push, rather than assuming
the behavior from a version number. Keep its total wall time, observation-probe
time and derived mutation interval distinct; the probe can affect scheduling.
This component excludes the outer recovery journal, which the native cleanup
scenario includes. Validate against the CI Git version when changing these guards.
