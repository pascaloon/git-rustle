# Isolated application screenshots

Run `screenshot-app.ps1` from the repository root with PowerShell 7, Git, and
a built Windows executable. It captures only Git Rustle’s window on a private,
non-current desktop. Run captures separately from performance measurements.

```powershell
.\scripts\screenshot-app.ps1
# Or select views and an alternate build:
.\scripts\screenshot-app.ps1 -Candidate .\target\release\git-rustle.exe `
  -Scenarios @('graph','settings','stash-panel') `
  -OutputDirectory .\target\bench\my-visual-review
```

Choose a new output directory. The default run covers the graph, branches,
settings, a diff, and a populated stash list. For each view, the runner creates
a disposable repository, isolates app data and Git configuration, waits for
the expected paint, and captures the window. It then checks the fixture’s
files, HEAD, refs, status, and stashes.

| Artifact | Purpose |
| --- | --- |
| `<scenario>/screenshot.png` | Full client pixels at the window's physical resolution |
| `<scenario>/screenshot.json` | Dimensions, DPI, app identity, desktop identity, hash, capture duration and pixel sanity results |
| `<scenario>/readiness.json` | Actual application paint mark observed before capture; not a model snapshot |
| `<scenario>/app.log` | Application trace and errors |
| `<scenario>/fixture-before.json`, `fixture-after.json` | Repository postconditions |
| `<scenario>/capture-*.json` and logs | Worker request and diagnostics, retained on failure |
| `report.json` | Overall success/failure, per-view results, application binary hash and limitations |

Open the PNGs with a local image-reading tool. Check the intended state, complete
right/bottom edges, readable text, spacing, truncation, contrast, control grouping
and consistency with adjacent views. Record the findings with the change. A PNG
passing the pixel checks is not automatically an acceptable UI. Images from
different runs may differ due to paths, dates, asynchronous activity or fonts;
do not use byte equality as a general visual regression oracle.

## Appearance coverage

`Settings → General → Appearance` offers **Black**, **Dark** and **Light** in a
segmented control. Black is the default and retains the original pure-black
palette; Dark uses charcoal surfaces inspired by VS Code, orange accents and
darker inset input fields.
Switching updates the entire native UI immediately and saves the preference
globally in `git-rustle/appearance.json` under LOCALAPPDATA. Existing `light_theme`
preferences migrate to Black or Light without changing the saved choice.
Appearance writes run on the background executor; a failed save restores the
preceding theme and shows an
error below the segments.

Run the complete view matrix at normal and minimum window sizes for each palette
(the existing runner name is retained for compatibility):

```powershell
.\scripts\validate-light-theme.ps1 -Candidate .\target\release\git-rustle.exe `
  -Theme dark -OutputDirectory .\target\bench\dark-theme-visuals
# Repeat with -Theme black and -Theme light in separate output directories.
```

The matrix includes the welcome screen, graph and commit inspector, pending and
syntax diffs, flat/hierarchical branches, stashes, cleanup, every Settings page,
external-tool configuration, request composer/selectors, rebase plan, branch/tag/
checkout dialogs, graph menus, and AI settings/draft/summary/conflict/error views.
Each group uses the existing action/paint harness and preserves its disposable
fixture. `coverage.json` retains failures while allowing independent views to
continue. Inspect every generated PNG; a successful scenario is not visual review.
Scrollable views may show only the current viewport. Remote/provider fixtures are
local and do not establish live service behavior.

For a focused capture, add `-Theme black`, `-Theme dark` or `-Theme light` to
`screenshot-app.ps1`; the old `-LightTheme` flag remains an alias for Light. The
`theme-toggle` scenario always starts Black and invokes the real segment actions
through all six directed theme transitions, awaiting persistence and native paint.
It asserts palette/component mode, the selected segment and non-overlapping
segment bounds inside the Appearance card. It then injects a failed save in
marked disposable settings, checks rollback and error state, removes the failure
and retries successfully. It finishes in Dark. Captures use
isolated application data and never alter the user's saved appearance. The
welcome fixture uses an owned temporary directory outside this checkout because
Git discovers repositories in parent directories, then removes that directory.

Keep capture runs separate from `benchmark-desktop.ps1 -Scenarios theme-toggle
-Samples 15` timing runs. The trace records action-to-paint-and-persistence
durations for the six successful transitions per launch; failure/retry checks are
excluded from those durations. Shared startup, normal/searched scrolling, diff scrolling
and General tab frames should also be compared against a preserved baseline.
`benchmark.ps1 -Theme <choice>` requires `-IsolatedCacheRoot` for non-default
themes. It seeds both binaries with the requested theme and a legacy Light flag;
the previous two-theme baseline uses Black when comparing the new Dark palette.
Desktop, graph, remote-checkout and branch-cleanup runners also accept `-Theme`
for their isolated fixtures. Screenshots must remain separate from timing runs.

## Additional scenarios

AI scenarios exercise real application handlers with deterministic external CLI
executables; they do not contact a provider or require authentication:

```powershell
.\scripts\screenshot-app.ps1 -Candidate .\target\debug\git-rustle.exe `
  -Scenarios @('ai-settings','ai-commit','ai-summary','ai-rebase','ai-rebase-cancel','ai-rebase-error','ai-merge','ai-merge-cancel','ai-merge-error','ai-conflict','ai-conflict-rebase','ai-conflict-pull','ai-stale','ai-cancel','ai-error') `
  -AiProvider Codex -OutputDirectory .\target\bench\ai-codex
# Repeat with -AiProvider Claude and a new output directory.
# Add -LegacyClaude with the Claude fixture to test older CLI flag fallback.
# Use -WindowSize 900x600 for AI layout checks at the minimum supported size.
```

The runner compiles `scripts/ai-cli-fixture.rs` into its ignored output directory,
prepends only that fixture directory to PATH, seeds isolated global AI settings,
and retains the submitted context. Native assertions wait for worker completion
and actual GPUI paint bounds. They verify selected-file messages, editable
resolutions, whole-diff summaries including unchecked/search-hidden files,
summary patches larger than the former 256 KiB cap reaching the CLI intact,
automatic rebase completion across commits/files, automatic merge
completion with both parents preserved, global settings
saves, cancellation, stale drafts and CLI errors. Every AI scenario asserts that
the footer is the app's single 32-pixel status bar. Read-only scenarios check
file contents, HEAD, refs, index and stashes remain unchanged. Automatic integration
scenarios instead verify ancestry, resolved files, clean tracked state,
unchanged unrelated refs/files/stashes, or preserved paused state after failure
or cancellation. These checks cover integration plumbing; live authentication, model
availability, provider responses and physical input still require separate use.

`request-branches` and `request-branches-menu` cover the PR/MR branch selectors.
Captures may omit background text even after a full repaint; this also occurs
in baseline views. Retain and report that PNG. `-DisableDirectComposition` allows
a separate diagnostic capture; it can improve the result but does not guarantee
a complete underlay. This still uses PrintWindow on the helper-owned private
desktop. Record the renderer choice, inspect every PNG, and do not treat the two
captures as identical rendering conditions or as proof of physical-display output.

Reuse the existing real action/paint harness for new views. Add a preset and a
matching disposable fixture to `screenshot-app.ps1`, or call the capture helper
from a dedicated visual scenario runner:

```powershell
. .\scripts\benchmark-screenshot.ps1
# Configure isolated app data and a marked disposable fixture, and enable trace.
# Start-BenchmarkNativeProcess is imported by benchmark-screenshot.ps1.
$app = Start-BenchmarkNativeProcess -FilePath $candidate `
  -ArgumentList @('"' + $fixture + '"','--no-cache') `
  -RedirectStandardError (Join-Path $run 'app.log')
try {
    # Wait with a timeout for the scenario's actual GPUI paint acknowledgement.
    # Keep AUTOQUIT long enough for capture; a process that already exited fails.
    Save-BenchmarkNativeScreenshot -Process $app `
      -OutputPath (Join-Path $run 'screenshot.png') -TimeoutSeconds 15
    # Continue scenario assertions, or pass -CloseAfterCapture on the final image.
} finally {
    $app.Dispose()
}
```

The helper accepts only a process object from the private-desktop launcher. There
is no arbitrary HWND or current-desktop capture option. Multiple snapshots are
possible with different output names while the app remains alive. For transient
states, add a scenario checkpoint that keeps that state until capture completes;
do not rely on a fixed sleep to catch a menu, spinner or confirmation. Launch the
capture worker outside the app's UI thread. The generic runner covers settled
views, not every intermediate interaction.

## Capture contract and limitations

The worker verifies the process creation timestamp, enumerates only the named
private desktop, requires exactly one visible owned app window, and rechecks
ownership and desktop membership around capture. It retains access to that
desktop until all window operations finish. Per-monitor thread DPI awareness
prevents logical-pixel clipping; bounds must remain stable during capture.

Capture uses [PrintWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-printwindow)
with client-only/full-content flags into an opaque bitmap. The call is
synchronous, so it runs in a separate hidden worker with a timeout. A sentinel
background and sampled-color checks reject obviously blank or unpainted output.
Existing evidence is never overwritten. Failed requests retain diagnostics and
the runner disposes its app process and private desktop. The lower-level helper
disposes its worker; its caller must always dispose the app in `finally`.

PrintWindow support depends on the renderer and Windows environment. Success and
pixel variation do not prove every element was captured; inspect the PNG. The
helper fails if it cannot verify the input desktop (which can happen in locked
or restricted sessions), sees ambiguous app windows, or cannot capture pixels.
There is no screen-capture, desktop-switch or visible-window fallback. These are
app client images, excluding OS window decorations, physical input, interactive
desktop composition, display scanout and the subjective feel of interaction.

Capture forces rendering work and includes GPU synchronization. Keep screenshot
runs entirely separate from performance comparisons. `captureMs` measures only
the native capture call and ownership verification before it; worker compilation,
image validation and PNG encoding are outside that interval. It is not an app
frame-time measurement. No capture code runs in normal Git Rustle launches.

Run the focused integration checks after changing these helpers:

```powershell
.\scripts\test-app-screenshot.ps1
```

They exercise two real views, decode PNGs, check metadata and fixture preservation,
and reject exited processes, existing output, non-private desktops and stale PID
identities. They do not replace inspecting images or the functional test suite.

The capture metadata also records the owned app's native window title through
`GetWindowTextW`, after the same PID and private-desktop ownership checks. This
validates project titles even though the PNG deliberately excludes window chrome.
The feature-specific runners listed in [benchmarking](benchmarking.md) extend
capture coverage to hierarchy, cleanup, multi-selection, diff search, pipelines
and external-tool icons. Their screenshot runs remain separate from timing.
