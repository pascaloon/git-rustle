# Default agent validation workflow

These instructions apply to all agents and delegated subagents working in this
repository. Include this workflow when handing off work.

Validate autonomously using code, CLI tools, disposable Git repositories, and
automated application scenarios. Do not use Computer Use, control the user's
mouse or keyboard, capture their desktop, switch desktops, or request interactive
UI testing unless the user explicitly asks for that interaction. Routine feature
implementation and validation do not imply permission for Computer Use.

For native UI checks, use `scripts/benchmark-native-process.ps1` to run the real
application on a separate, non-current Windows desktop. Invoke real application
action handlers and await worker results and GPUI paint callbacks. Assert relevant
layout bounds, rendered state, and repository postconditions rather than merely
checking that the process launched. Extend the existing scenario harness when a
feature lacks coverage. Keep benchmark helpers and external fixture tools hidden,
and clean up owned processes and private desktops on completion or timeout.

For visual changes, also capture and inspect actual application PNGs using
`scripts/screenshot-app.ps1` (see [the screenshot workflow](docs/app-screenshots.md)).
This captures only the helper-owned app window on its non-current desktop; it
does not use Computer Use or capture the user's desktop. Review the images with
a local image-reading tool for clipping, spacing, typography, colors, and state
clarity. Pixel sanity checks alone are not visual review. Extend relevant
automated scenarios when the changed view is not covered. Keep screenshots and
metadata under ignored `target/bench/<run>` and record findings and limitations.
Run screenshots separately from performance samples because capture changes
rendering timing. Never fall back to screen capture if window capture fails.

Inspect older runners before launching them: some still open native windows on
the user's desktop. Adapt those launches to the private-desktop helper first.
Do not silently fall back to visible windows or Computer Use if isolation fails;
continue independent checks and report the specific validation gap. Hiding or
minimizing a GPUI window is not a substitute for verified native paint.

Use disposable, clearly marked fixtures and isolated application data for
mutating tests. Verify actual files, index, commits and stashes as appropriate,
including preservation of unrelated changes and Undo/Redo where applicable.
Run relevant functional tests, formatting and lint checks. For performance,
follow the requirements below and the linked workflow.

Report exactly what was verified and any limitations or unresolved regressions.
Automated action and paint checks do not establish physical input behavior,
interactive desktop composition, display scanout, or human UX judgment. Never
describe automated assertions as manual visual testing. When PNGs were inspected,
report screenshot review separately from behavioral and performance validation.

# Performance expectations

Performance is a primary product requirement. Benchmark notable changes using
[the benchmark workflow](docs/benchmarking.md), preserving a baseline executable
before changing code. Measure the affected operation as well as startup when
relevant; startup alone cannot establish that remote previews or scrolling stay
fast. Record raw samples and a Markdown report with results and limitations under
the ignored `target/bench/<run>` directory. Keep generated benchmark artifacts out
of version control; commit only benchmark instructions, scripts, and harness code.

Documentation-only changes do not require performance runs. Previous measurements
are not substitutes for measuring a new implementation.
