# Isolated application screenshots

Agents should inspect actual screenshots when changing visual UI. The runner
captures Git Rustle's client area on a private, non-current Windows desktop. It
does not switch desktops, activate windows, move the pointer, send keyboard input,
or capture the user's screen. It needs Windows, PowerShell 7, Git and a built
Git Rustle executable; no extra screenshot package or product build flag is needed.

From the repository root:

```powershell
.\scripts\screenshot-app.ps1
# Or select views and an alternate build:
.\scripts\screenshot-app.ps1 -Candidate .\target\release\git-rustle.exe `
  -Scenarios @('graph','settings','stash-panel') `
  -OutputDirectory .\target\bench\my-visual-review
```

The output directory must be new. Defaults cover graph, branches, settings, diff,
and the populated Stashes tab. Each view gets a fresh disposable Git fixture,
isolated application data and Git configuration, and a separate native app
process. Fixtures contain six commits, two branches, a tag, three stashes and
pending files. There are no remotes, hooks or signing prompts. The runner waits
for the application's first-frame or scenario-paint trace, then captures the
window and requests normal application close. It checks that files, HEAD, refs,
status and stashes are unchanged. The resulting files are:

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

## Additional scenarios

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
