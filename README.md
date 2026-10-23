# Git Rustle

Fast, Windows-native, GPU-accelerated Git client, written in Rust with GPUI.
The desktop renderer uses DirectX on Windows. Git history, diffs, search, remote
previews, checkout and commits keep the existing Gitoxide/Git worker implementations.
The native UI uses an OLED-black canvas, charcoal panels and Rust-orange accents,
with separate green/red diff status colors.

## Build and launch

Requires Windows 10/11, a DirectX-capable graphics driver, a current stable Rust
MSVC toolchain, and Visual Studio C++ Build Tools. Git for Windows on PATH is
required for Git mutations and recovery. No terminal host is needed.

```powershell
cargo build --release --locked
.\target\release\git-rustle.exe [PATH]
```

For faster optimized builds during development, use the `fast` profile:

```powershell
cargo build --profile fast --locked
.\target\fast\git-rustle.exe [PATH]
```

It inherits the release settings with LTO disabled and 16 code-generation units.
The first build creates a separate dependency cache. Use the release profile for
shipping and runtime performance validation.

`PATH` can be a repository or a directory inside it; the default is the working
directory. `--no-cache` disables persistent application caches. `--help` and
`--version` are also available. When the directory is outside a repository, the
landing page offers **Open Directory** (the native Windows folder picker) and the
20 most recently opened repositories. Click a repository to reopen it, or **×**
to forget it. History is shared by all Git Rustle instances for the current user
and stored in `%LOCALAPPDATA%\git-rustle\recent-repositories.json`.

The Windows executable embeds the approved windblown-leaf artwork from
`assets/app-icon.png`, with transparent corners; GPUI uses the same icon for the
native window and taskbar.
The checked-in `assets/app-icon.ico` contains sizes from 16 to 256 pixels. To
regenerate it after replacing the square source image, install Pillow and run
`python scripts/generate-app-icon.py`. Normal builds do not require Python.
Run `python scripts/remove-icon-matte.py INPUT.png OUTPUT.png` on the original
opaque artwork to remove its corner-connected white matte while preserving
interior pixels. The icon generator retains the resulting alpha channel.

## Desktop workflow

- Click a history row to inspect its full message, author, committer, dates,
  parents and hash. The inspector's **Copy SHA1** icon copies the full commit hash.
  Drag to select details text and use Ctrl+C to copy.
- Use **HEAD** to locate the checked-out commit. Dirty repositories initially
  select **Pending changes**; clean repositories initially select HEAD.
- Graph ordering and lanes stay stable when checking out commits already in the
  reference history. Detached commits outside that history appear after it;
  **HEAD** still locates them. The selected and hovered branch paths have a thin
  light outline, preserving their branch colors.
- Ctrl+click selects several commits. Each selected branch path keeps its light
  outline, and SHA labels on those paths have a light background. Selected commit
  details stack as expandable cards; open a card to load its full message and
  metadata.
- **Checkout** opens the local branch directly when a commit has one logical
  branch. Its matching upstream is an alias; distinct branches still show a
  chooser. Commits without branches retain detached-checkout confirmation.
- **Diff against branch base** compares the selected commit with its merge base
  against the remote default branch, or `main`/`master` when available. If no
  default is known, choose a base branch. The comparison uses the common ancestor,
  so changes added independently to the base branch are excluded.
- Right-click a graph row for **Diff commit** and **Checkout…**. Ctrl+click
  toggles additional commits; the context menu compares exactly two selected
  commits and disables Checkout for multiple selections. Double-click opens a
  commit against its first parent. A normal click returns to a single selection.
- **Git Graph**, **Branches**, **Stashes**, **Cleanup**, **Diff** and **Settings** are tabs. The Diff tab
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
- Source diffs gain syntax colors automatically while remaining immediately readable.
  Coloring covers common languages including Rust, C/C++, C#, JavaScript/TypeScript,
  Python, Go, Java, Ruby, shell, HTML/CSS, JSON, TOML, YAML, SQL and Markdown.
  Large/generated files and unsupported formats keep plain text. See
  [syntax highlighting](docs/syntax-highlighting.md) for coverage and limits.
- **Branches** opens the activity-sorted, filterable branch browser.
  Switch between List and Hierarchy to group slash-separated branch names into
  folders. A local branch and its upstream share a row when their tips match.
  Double-click a branch to locate its tip or load a remote-only preview.
- **Cleanup** analyzes local branches matching their upstream and local-only
  branches whose last commit is at least six months old. Review author/date and
  selections before deleting; current/worktree branches are excluded and default
  branches start unchecked. Remote deletion is separately opted into and cannot
  be undone locally. **Optimize storage** consolidates objects while retaining
  recovery and unreachable commits, and reports Git's measured storage change.
- **Refresh** / **F5** checks local refs/status and current server tips for the
  branches in the loaded Graph, including their upstreams, fetched remote refs
  and explicitly previewed branches. It retains reusable caches and skips full
  server branch lists, tags, provider metadata and stashes. Unchanged tips retain
  the existing Graph, selection and search. Slow server checks finish in the
  background with progress visible.
- **Ctrl+click Refresh** / **Ctrl+F5** performs a **Force Refresh** of all views,
  including complete server branch lists, tags, provider metadata and stashes.
  Immutable commit, file and image caches remain reusable.
- **Pull**, **Push**, and **New branch** live in the graph toolbar. Push reviews
  the current branch with a green, orange or red safety shield. Right-click a
  commit to merge, rebase, or rename/delete its local branches.
- **Interactive rebase from here…** prepares an editable commit plan from the
  graph menu. Its tab appears only for an open plan or active interactive rebase.
  Reorder, reword, squash, fixup, edit and drop are supported. **Conflicts** opens
  the resolution workspace for paused operations.
- **Undo / redo history** in the bottom-right status bar lists recoverable states.
  Double-click a state to move backward or forward. Commit and checkout share
  this history; pushes and outside repository changes invalidate earlier states.
  See [Git operations and recovery](docs/git-operations.md) for safety boundaries,
  supported repository types and benchmark commands.
- **Settings → General** contains the website/release links, image-cache reset
  controls and **Check for updates**. External tools show their executable icons
  here and as quick launch buttons in diff/conflict views. The Project, GitLab and
  GitHub sections configure provider API access; GitLab tokens are shared across
  repositories on the same API installation.
- Remote/server branch labels show merge requests once and the latest branch-head
  pipeline status: gray pending, blue running, green success, yellow warning or
  red failure, with distinct icons and status tooltips.
- The shell's folder and terminal icons open the repository in Explorer or
  Windows Terminal. Windows Terminal must be installed for its launcher.

The former terminal navigation keys are not bound in the desktop. Normal text
editing, selection, clipboard and input-method behavior remain available in fields.

### Search

Ctrl+F focuses the current graph, branch or diff search. Escape clears an active
filter; in Diff, another Escape returns to the graph. Diff search matches filenames
and both versions' full text in a cancellable background worker. Results include
line previews and jump to displayed hunks; unchanged context outside the hunks
remains available in the preview. Binary/unavailable files and contents exceeding
8 MiB per side or the 64 MiB index budget are filename-only. The window title
includes the open project name.

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

The branch filter matches branch names and the latest commit author shown in each
row, ignoring case. Both search fields retain a dim orange border and background
while filtering, even after focus moves elsewhere. The graph marks the checked-out
commit with a high-contrast orange **HEAD** badge.

PR/MR badges in the graph are informational. Use the commit context menu to open
the associated request online.

### Pending changes and commits

The Pending changes row includes staged, unstaged and untracked files, excluding
ignored files. Status refreshes in the background every two seconds. The composer
accepts a multiline description and optional new branch name. File/folder
checkboxes and **Check all** / **Uncheck all** select whole files, using their
current working contents. **Commit** submits the checked files. Unchecked files
and their staged contents are preserved. Drafts and checks survive viewing diffs.

Right-click a file in the pending file list and choose **Discard file** to arm
the action. The item turns red and changes to **Confirm discard file — click
again**; click it again to restore that file to HEAD, or remove it if it is new.
This discards both its staged and unstaged changes while preserving other files.
Click elsewhere or press Escape to cancel. Undo / redo history can recover the
discarded contents.

Right-click the pending graph row and choose **Stash changes…** to open the
Stashes tab. Enter an optional name and click **Stash changes** to save staged,
unstaged and untracked files. Apply keeps the stash entry; Delete removes it and
can be recovered through Undo / redo history. The tab follows external stash
changes and the global Force Refresh action.

A branch name creates and switches to a new branch from HEAD; existing names are
rejected. A blank field commits on the current branch or detached HEAD. Empty
repositories are supported. Git identity, signing and hooks apply. Hooks that
stage unchecked paths are rejected. Conflicts and ongoing merge/rebase/cherry-pick
operations must be resolved first. Failures preserve the draft for retry. Closing
the window waits for an active commit, checkout or settings save to finish.

### Conflicts

The Conflicts workspace shows **Incoming** commits on the left and **Local**
commits beside them, using the graph's commit rows. Select a commit to inspect its
message, author, dates and changed files in the shared inspector on the right.
During a rebase, Incoming is the current base and Local is the commit being replayed.

Below the commit columns, select a conflicting file to load its versions and
builtin result editor. File contents load only for the selected file. **Use
incoming…** and **Use local…** confirm a whole-file replacement and stage it.
For text files, edit the result and choose **Save and mark resolved**. **Mark file
resolved** stages the existing working-tree file. Binary, large and non-text files
can use a whole version or an external application; configured merge tools are
optional.

The workspace automatically detects external file and index changes; no Refresh
button is needed. Unsaved editor drafts survive file switches and external
changes. Saving checks the original file and operation identity, rejecting stale
edits rather than overwriting outside changes. **Discard draft & reload** explicitly
replaces a stale draft with the current file.

The bottom action bar keeps **Abort…** immediately beside **Continue**. Continue
remains disabled until all conflicts are resolved and staged. **Skip commit…**
is available for operations that support it. A conflicted stash apply keeps its
stash entry; **Finish applying stash** completes the resolution, while Abort
retains recovered untracked files.

### Remotes

GitHub and GitLab branches, tags, missing commits and remote diffs load through
read-only provider APIs. Graph refreshes check branch tips through the provider's
HTTPS ref advertisement, reusing connections and the existing provider token.
Git's normal transport (including SSH) and branch APIs remain fallback paths.
Browsing and previews do not fetch Git objects or alter
repository refs/worktrees. Startup checks configured local upstreams and fetched
remote-tracking branches; opening the branch browser requests the full listing.
Remote enrichment prioritizes selected
branches and bounds background work. Activity uses the latest 100 first-parent
commits and counts commits from the past seven days.

Blue `origin/branch` graph badges stay on the locally fetched tracking refs.
When the provider reports a different tip, a separate blue badge with the same
name and a cloud icon marks that tip. Matching tips share one badge.

Ghost previews preserve merge ancestry and selection. Immutable metadata, trees,
file contents and assembled previews are cached separately; branch/tag caches
have a five-minute TTL. Smart Refresh always verifies Graph branch tips;
Force Refresh also revalidates the full branch and tag lists. Cached results remain usable
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
hosts, use **Settings → Project** to select the Git remote, provider and project
path. **GitLab** and **GitHub** each have a custom server/API URL and access token.
These two tabs apply globally to the Windows user across projects; their URLs are
stored in `%LOCALAPPDATA%\git-rustle\providers.json`. A global provider URL takes
precedence over older repository API URL overrides. Tokens are saved per API
installation in Windows Credential Manager, shared across its projects, and shown
as a mask when present. Leave the mask unchanged to keep a token, or type to replace
it. **Forget saved token** removes the saved credential on Save. Changing the URL
requires a new token or explicitly forgetting the old one when a token is saved.
Each subtab keeps an independent draft. Git transport URLs remain unchanged.

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

See [the benchmark workflow](docs/benchmarking.md) and the
[additional benchmark helpers](docs/benchmarks/README.md). Keep generated samples,
logs, screenshots and reports in ignored `target/bench/<run>` directories.
Terminal render/input adapters are retained as regression references for existing
tests, but the executable enters GPUI and never opens a console input loop.

### Native diagnostics

- `GIT_RUSTLE_TIMING=1`: report internal timing milestones on stderr at exit.
- `GIT_RUSTLE_TRACE=1`: additionally emit those milestones as they happen.
- `GIT_RUSTLE_AUTOQUIT=<ms>`: exit after the first history frame and loading settles.
- `GIT_RUSTLE_DUMP=<path>`: save a final model summary, not a screenshot.
- `GIT_RUSTLE_SCENARIO=scroll|graph-resize|search-scroll|files-scroll|diff-scroll|diff-pan|wrap-scroll|tabs|branches|diff|compare|compare-menu|search|settings|settings-tabs|pending|pending-diff`: run a non-mutating
  desktop benchmark action after loading.
- `GIT_RUSTLE_FRAME_SAMPLES=<path>`: save 120 scroll frame intervals after discarding
  one warm-up frame. These include scheduling/layout/paint submission and exclude
  input-device latency; they are not GPU presentation timestamps.

## Windows CI artifacts

Manual runs of `.github/workflows/windows-release.yml` build and test the Windows
x64 executable and retain an Actions artifact for 90 days. To publish a release,
update the package version in `Cargo.toml` and `Cargo.lock`, commit it, then push
the matching tag (for example `v0.2.0`). The workflow rejects a tag that differs
from the compiled version. After tests and smoke checks pass, it uploads
`git-rustle-x86_64-pc-windows-msvc.exe` and `SHA256SUMS` to a draft GitHub Release,
then publishes it with generated release notes. Tags containing a prerelease
suffix publish prereleases. Published assets are never overwritten by reruns;
failed draft uploads can be retried. Build duration is separate from runtime performance.

### Application updates

**Settings → General → Check for updates** checks immediately, independently of
the scheduled timer, and shows whether the app is current, an update is available,
or the check failed. It does not install an update automatically.

After its first frame, Git Rustle checks the public `pascaloon/git-rustle`
GitHub Releases feed on a background worker on every launch. While open, the app
checks again six hours after each successful check. The last successful result is
recorded in `%LOCALAPPDATA%\git-rustle\updates.json` for diagnostics and never
suppresses a fresh check. Failed checks retry after an hour while the app remains
open. Offline failures and rate limits stay quiet.
Only a newer stable version with the exact Windows x64 asset and a valid GitHub
SHA-256 digest produces the orange **Update available** button at the left of
the status bar (including the welcome screen).

Click the button and choose **Update and restart**, or **Later**. The app waits
for active Git operations, request submissions and settings saves. `self_update`
downloads the offered version, verifies its pinned SHA-256 digest, and replaces
the executable using its Windows helper. Closing is deferred during installation.
The new process reopens the current repository with the commit message,
new-branch field and file selections; other unsaved forms close as stated in the
confirmation. If restart fails, the app remains open with a retry action.
Multiple instances serialize installation and refuse downgrades. The executable
directory must be writable; protected installations require a manual update.

Checks and downloads use HTTPS without a bundled GitHub token. Digests protect
against corrupt or changed downloads; they are not publisher signatures. This
release flow does not currently add Authenticode or an independent signing key.
GitHub repository/release permissions therefore remain the update trust boundary.
An initial version containing this updater must be installed manually before
subsequent releases can be installed from inside the app.

## License

Git Rustle is licensed under the GNU General Public License version 3 only
(`GPL-3.0-only`). See [LICENSE](LICENSE) for the full terms.

This program is distributed without any warranty, including the implied
warranties of merchantability or fitness for a particular purpose, to the extent
permitted by applicable law. See the license for details.
