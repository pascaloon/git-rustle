# Git Rustle

Fast, Windows-native, GPU-accelerated Git client, written in Rust with GPUI.
The desktop renderer uses DirectX on Windows. Git history, diffs, search, remote
previews, checkout and commits keep the existing Gitoxide/Git worker implementations.
The native UI uses an OLED-black canvas, charcoal panels and Rust-orange accents,
with separate green/red diff status colors.

## Build and launch

Requires Windows 10/11, a DirectX-capable graphics driver, a current stable Rust
MSVC toolchain, and Visual Studio C++ Build Tools. Git for Windows on PATH is
required for checkout and commit creation. No terminal host is needed.

```powershell
cargo build --release --locked
.\target\release\git-rustle.exe [PATH]
```

`PATH` can be a repository or a directory inside it; the default is the working
directory. `--no-cache` disables persistent application caches. `--help` and
`--version` are also available. Launch the executable with a repository path when
opening it outside that repository.

## Desktop workflow

- Click a history row to inspect its full message, author, committer, dates,
  parents and hash. Drag to select details text and use Ctrl+C to copy.
- Use **HEAD** to locate the checked-out commit. Dirty repositories initially
  select **Pending changes**; clean repositories initially select HEAD.
- Right-click a graph row for **Diff commit** and **Checkout…**. Ctrl+click
  toggles additional commits; the context menu compares exactly two selected
  commits and disables Checkout for multiple selections. Double-click opens a
  commit against its first parent. A normal click returns to a single selection.
- **Git Graph**, **Branches**, **Diff** and **Settings** are tabs. The Diff tab
  appears after opening a diff and retains its file and scroll position when
  switching tabs. A newly opened comparison replaces the previous diff.
- Click folders to fold them and files to open their side-by-side diff. File and
  folder totals update as the background stats worker completes. **Expand** and
  **Collapse** icons operate on the whole tree. Choose files in the sidebar.
- Scroll lists using the wheel or scrollbar. Pan wide diffs horizontally. Drag
  the graph-column or history/details divider to resize. The icon above the diff
  toggles word wrap; copying preserves source lines rather than visual wraps.
  Scrollbars appear when hovering a
  scrollable region. Diff text supports dragging across lines on either side,
  double-click to select a displayed line, and Ctrl+C; copied text excludes line
  numbers and includes collapsed context. Commit descriptions and the selected
  file path also support selection and copying.
- **Branches** opens the activity-sorted, filterable branch browser.
  Double-click a branch to locate its tip or load a remote-only preview.
- **Checkout…** opens an explicit choice dialog. Clicking a branch or detached
  commit choice performs that checkout. **Cancel** performs no Git operation.
- **Refresh** reloads refs/status and revalidates remote data.
- **Settings** configures provider API access.
- The shell's folder and terminal icons open the repository in Explorer or
  Windows Terminal. Windows Terminal must be installed for its launcher.

The former terminal navigation keys are not bound in the desktop. Normal text
editing, selection, clipboard and input-method behavior remain available in fields.

### Search

Type in the search field to filter history. Matching is case-insensitive and
accepts contiguous fragments inside words. All space-separated terms must match
one field, in any order. Default search covers hashes, full commit messages,
authors, branch labels and tags. File paths require `f:`.
Focus the search field to choose a search prefix from its dropdown. Matching
text is highlighted in history, details and file names; branch filters highlight
matching names too.

| Query | Field |
| --- | --- |
| `f: .sharpmake.cs` | Changed paths, including both rename names |
| `a: Bob` | Author |
| `m: Bump thingy` | Full message |
| `s: dd4d723` | Commit hash |

Message/path indexes are built lazily on workers and reused across launches.
Results stream during indexing and retain ancestry through hidden commits.
Unavailable objects and remote errors are reported as incomplete results.
Click the field's **×** button or **HEAD** to leave the filter. The branch filter
also has a clear button.

### Pending changes and commits

The Pending changes row includes staged, unstaged and untracked files, excluding
ignored files. Status refreshes in the background every two seconds. The composer
accepts a multiline description and optional new branch name. File/folder
checkboxes and **Check all** / **Uncheck all** select whole files, using their
current working contents. **Commit** submits the checked files. Unchecked files
and their staged contents are preserved. Drafts and checks survive viewing diffs.

A branch name creates and switches to a new branch from HEAD; existing names are
rejected. A blank field commits on the current branch or detached HEAD. Empty
repositories are supported. Git identity, signing and hooks apply. Hooks that
stage unchecked paths are rejected. Conflicts and ongoing merge/rebase/cherry-pick
operations must be resolved first. Failures preserve the draft for retry. Closing
the window waits for an active commit, checkout or settings save to finish.

### Remotes

GitHub and GitLab branches, tags, missing commits and remote diffs load through
read-only provider APIs. Browsing and previews do not fetch Git objects or alter
repository refs/worktrees. Startup checks configured local upstreams; opening the
branch browser requests the full listing. Remote enrichment prioritizes selected
branches and bounds background work. Activity uses the latest 100 first-parent
commits and counts commits from the past seven days.

Ghost previews preserve merge ancestry and selection. Immutable metadata, trees,
file contents and assembled previews are cached separately; branch/tag caches
have a five-minute TTL. Refresh revalidates them. Cached results remain usable
when requests fail. Remote pairs can compare divergent branches, local/remote
endpoints or separate provider endpoints. Truncated remote trees produce errors
rather than incomplete comparisons. Provider limits and unavailable ancestry
remain visible. Previews are limited to 5,000 additional commits, 100 requests
and 60 seconds plus the active request per remote, and 10,000 tags per remote.

Checkout uses Git's normal overwrite, worktree and in-progress-operation
protections. It never force-resets, cleans or automatically stashes. Local tracking
branches are reused; otherwise checkout creates the appropriate tracking branch.
Missing objects are fetched only for the selected target. Detached ghost checkout
requests that commit specifically. Conflicting names are reported. API operations
have 12-second request timeouts; Git operations have a five-minute timeout.

### Provider configuration

Public GitHub/GitLab HTTPS, SSH and SCP-style URLs are recognized. For on-premise
hosts, use Settings to choose GitLab/GitHub, select a configured Git remote, enter
the API base and project path, and **Test** or **Save**. Provider tabs retain
independent drafts. Tokens are masked and saved in Windows Credential Manager;
they are not written into application caches. **Forget saved token** removes the
saved credential on Save. Git transport URLs remain unchanged.

## Performance and validation

History, branches, changed files and diff lines use virtualized lists: only visible
rows are constructed. Graph lanes and diff selections are GPU-painted. Repository,
network, status, commit, checkout, search, diff and stats work use background
workers. Commit details use a coalescing worker with a bounded 64-entry cache.
Repository loading/layout overlaps DirectX initialization. Idle windows are not
continuously redrawn. Existing packed history caches and
immutable remote caches remain in `%LOCALAPPDATA%\git-rustle\cache`.

```powershell
cargo test --release --locked
cargo clippy --all-targets -- -D warnings
```

See [the benchmark workflow](docs/benchmarking.md) and
[the GPUI migration report](docs/gpui-benchmarks.md), followed by
[the visually verified usability fixes and measurements](docs/gpui-usability-benchmarks.md)
and [scrollbar/diff fixes and measurements](docs/gpui-scroll-benchmarks.md).
The [selection, search and layout report](docs/gpui-design-benchmarks.md) and
[tabs, context menus and word-wrap report](docs/gpui-tabs-benchmarks.md)
cover the revised desktop workflow. The [OLED theme report](docs/oled-theme-benchmarks.md)
records native visual checks and before/after startup and scrolling measurements.
The [commit inspector and graph contrast report](docs/details-contrast-benchmarks.md)
covers the structured sidebar, continuous text selection and distinct graph lanes.
Historical terminal reports
remain in `docs/`; they do not measure the desktop implementation. Terminal
render/input adapters are retained as regression references for existing tests,
but the executable enters GPUI and never opens a console input loop.

### Native diagnostics

- `GIT_RUSTLE_TIMING=1`: report internal timing milestones on stderr at exit.
- `GIT_RUSTLE_TRACE=1`: additionally emit those milestones as they happen.
- `GIT_RUSTLE_AUTOQUIT=<ms>`: exit after the first history frame and loading settles.
- `GIT_RUSTLE_DUMP=<path>`: save a final model summary, not a screenshot.
- `GIT_RUSTLE_SCENARIO=scroll|graph-resize|search-scroll|files-scroll|diff-scroll|diff-pan|wrap-scroll|tabs|branches|diff|compare|compare-menu|search|settings|pending|pending-diff`: run a non-mutating
  desktop benchmark action after loading.
- `GIT_RUSTLE_FRAME_SAMPLES=<path>`: save 120 scroll frame intervals after discarding
  one warm-up frame. These include scheduling/layout/paint submission and exclude
  input-device latency; they are not GPU presentation timestamps.

## Windows CI artifacts

Tag pushes and manual runs of `.github/workflows/windows-release.yml` build and
test the Windows x64 release with the locked dependency graph, smoke-test command
line flags, and upload `git-rustle.exe` as an unzipped Actions artifact for 90 days.
Build duration and size are recorded separately from runtime performance.
