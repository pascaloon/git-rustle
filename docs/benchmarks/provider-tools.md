# Provider badges, pipelines and executable icons

Preserve exact application and test baselines before changing code. After building
the final candidate application and test harness, run:

```powershell
.\scripts\benchmark-provider-tools.ps1 `
  -Baseline .\target\bench\my-run\baseline.exe `
  -BaselineHarness .\target\bench\my-run\baseline-tests.exe `
  -Candidate .\target\release\git-rustle.exe `
  -CandidateHarness .\target\bench\my-run\candidate-tests.exe `
  -OutputDirectory .\target\bench\my-run\provider-tools -Samples 15
```

The runner creates disposable eight-commit repositories without provider remotes,
isolated application settings and two configured tool fixtures: the preserved
application executable supplies an actual resource icon, and a missing executable
exercises the generic fallback. Neither configured tool is launched. It invokes
real application actions, waits for workers and native paint callbacks, and uses
the private-desktop helper without input, desktop switching or screen capture.
It checks files, index, HEAD, refs and stashes after every launch.

Shared native controls are `provider-scroll` and `provider-menu`, alternating
baseline/candidate order with one warmup and 15 retained launches per group.
Candidate provider fixtures require all five pipeline icon shapes to paint and
deduplicated provider destinations. The component tests verify exact latest-head
SHA matching and cached lookups. `tool-icons` opens General settings;
`tool-icons-diff` opens the real Diff view and its quick tool buttons. Both require
a decoded executable image, fallback icon and actual layout/paint bounds. These
new tool scenarios are candidate-only.

Optional harness inputs run provider/token/icon functional tests, the preserved
provider lookup benchmark, and candidate pipeline snapshot/icon extraction
benchmarks. Provider HTTP tests use loopback only. Credential tests use unique
synthetic API installation names with cleanup. The executable icon cache is
invalidated by path/file metadata, caches extraction failures, and reuses the
same image allocation on hits. No icon extraction or provider request occurs
during rendering.

PNG capture runs separately after all timing samples. Inspect those local files
for complete edges, readable names, distinct pipeline shapes/colors and icon
alignment; pixel sanity and paint assertions do not establish visual quality.
Raw logs, frame arrays, metadata, fixture fingerprints, binary hashes and the
median/nearest-rank-p95 report remain under the ignored output directory.

`wall_ms` is native application launch through graceful exit. `first_frame_ms`
and `scenario_frame_ms` are application trace timestamps. `operation_ms` spans
the scenario action request through its verified paint; scrolling includes 120
frame intervals. Component `worker_ms` includes loopback HTTP, parsing and cache
write; `cache_ms` includes cached snapshot reading/parsing. `extraction_ms` reads
the executable resource and constructs the bitmap; `cache_hit_ms` is the mean
per lookup across 1,000 hits, including file metadata validation. Do not compare
these different timing boundaries or infer a speedup for candidate-only cases.

Private-desktop evidence excludes physical input, interactive desktop composition,
display scanout and real provider latency. Pipeline warning icons represent
states such as manual, skipped or cancelled; the tooltip retains the provider's
status. GitHub Actions summaries aggregate the latest runs of workflows on the
same branch head. No claim is made about CI systems that do not expose these
provider APIs.

Pipeline snapshots retain at most 20 pages (2,000 records). Large projects keep
the valid bounded results, and unseen branch heads omit an icon. Every displayed
record must still match the latest discovered server head exactly. This bounded
coverage is not a guarantee that every historical workflow is represented.
