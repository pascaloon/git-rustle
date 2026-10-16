# git-rustle

A fast, colorful Git graph viewer for Windows Terminal, written in Rust.

git-rustle talks to the Win32 console directly (`windows-sys`) and emits VT
sequences itself — no ratatui, no crossterm. Commits are read with
[gitoxide](https://github.com/GitoxideLabs/gitoxide) and the graph layout is
cached under `%LOCALAPPDATA%\git-rustle\cache`, so the second launch on a
repository shows the full graph in a few milliseconds.

## Build

```powershell
cargo build --release
.\target\release\git-rustle.exe [PATH]
```

Requires Windows 10/11 with Windows Terminal (or any console host with VT
processing) and a stable Rust toolchain (edition 2024).

## Windows CI artifacts

Pushing any tag runs the **Windows release** GitHub Actions workflow. It builds
with stable Rust for `x86_64-pc-windows-msvc` using `cargo build --release --locked`,
runs release-mode tests, smoke-tests the executable, and uploads **only
`git-rustle.exe`** as a direct, unzipped Actions artifact retained for 90 days.
The existing release optimization settings are unchanged.

```powershell
git tag v0.1.0
git push origin v0.1.0
```

The tagged commit must contain `.github/workflows/windows-release.yml`.
Download the executable from the completed run under **Actions → Windows
release → Artifacts** (GitHub sign-in required). This is an Actions artifact,
not a permanent GitHub Release asset. The workflow can also be started manually
with **Run workflow** without creating a tag.

Each run records the release build duration, executable size, and SHA-256 in
its summary. Build duration measures CI throughput, **not** application runtime
performance; performance changes still require comparable Windows benchmarks.

## Usage

```
git-rustle [OPTIONS] [PATH]

  PATH          Repository (or any directory inside it); defaults to the cwd
  --no-cache    Ignore and do not write disk caches (graph and remote branches)
  -h, --help
  -V, --version
```

### Keys

| Key | Action |
| --- | --- |
| `↑` / `↓`, `k` / `j` | Move selection |
| `PgUp` / `PgDn` | Page |
| `Home` / `End`, `g` / `G` | First / last commit |
| `H` | Jump to `HEAD` |
| `b` | Browse, filter, and check out local or remote branches |
| `Tab`, `→` / `←`, `l` / `h` | Switch focus between graph and details |
| `[` / `]` | Shrink / grow the details panel |
| `Space` | Mark / unmark the selected commit (at most two) |
| `d` | Diff: the two marked commits, or the marked/selected commit against its parent |
| `Del` | Clear marks |
| `r` | Reload local refs and refresh the graph when they changed |
| `q`, `Ctrl+C` | Quit |
| `Esc` | Clear marks, or quit when none are set |

The details panel ends with a **Files** section: the tree of files changed by
the selected commit, each with `+added −removed` counts (folders total their
contents). `Tab` cycles graph → details → files; in the files tree `↑`/`↓`
move, `←`/`→` fold, `e`/`c` expand/collapse all, and `Enter` on a file opens
the full diff view at that file.

### Mouse

Click a row to select it, double-click to open its diff against its parent,
`Ctrl`+click to mark it, scroll the wheel over any panel, and drag the
splitter between the panels to resize them. In the files tree, click a folder
to fold it and double-click a file to open the diff view.

### Diff view

Press `d` (or double-click a commit) for a full-screen diff — of the two
marked commits, older against newer, or of one commit against its first
parent (root commits diff against the empty tree). A collapsible file tree
with per-file and per-folder line counts sits on the left; the right shows the
selected file side by side, with added/removed/changed lines coloured and the
changed span within a line highlighted. Exact renames are detected; binary and
very large files are summarised instead of diffed.

| Key | Action |
| --- | --- |
| `↑` / `↓`, `j` / `k` | Move in the file tree / scroll the diff |
| `←` / `→`, `h` / `l` | Collapse / expand folders (tree) or pan horizontally (diff) |
| `Enter`, `Space` | Toggle a folder, or jump to the diff for a file |
| `n` / `p` | Next / previous file |
| `e` / `c` | Expand / collapse all folders |
| `PgUp` / `PgDn`, `Home` / `End` | Page / jump |
| `Tab` | Switch focus between the file tree and the diff |
| `[` / `]` | Shrink / grow the file tree |
| `Esc`, `q` | Back to the graph |

Mouse: click a file to show it, click a folder to fold it, wheel to scroll
either panel (`Shift`+wheel or a tilt wheel pans), drag the splitter to resize.

### Remote branches and checkout

Press **`b`** to open the branch browser. Local branches and locally stored
remote-tracking refs appear immediately. GitHub and GitLab branch names and tip
IDs load through their read-only REST APIs on background workers. Startup queries
only local upstreams; opening `b` loads the full remote branch list. Browsing branches and ghost previews perform **no Git fetch**
and write no repository refs or objects.

| Key | Action in the branch browser |
| --- | --- |
| Type / `Backspace` | Filter branch names (including remote names) |
| `Ctrl+U` | Clear filter |
| `↑` / `↓`, `PgUp` / `PgDn`, `Home` / `End` | Select a branch |
| `Enter` | Check out the selected branch |
| `Tab` | Locate its tip in the local graph, if available |
| `F5`, `Ctrl+R` | Revalidate remote branches and reload local refs |
| `Esc` | Return to the graph |

After the local graph finishes loading, Git Rustle checks supported remotes in
the background. Only actual local branches and their configured upstreams qualify.
Standalone remote-tracking refs do not qualify. For eligible branches it retrieves missing
commit metadata through the provider API and connects it to the local graph.
Branches that exist only on the server remain available in `b` without adding
their history to the graph.

**Remote commits** retain the normal text, tag, hash, and author colors and node
shapes. Their branch lane and remote branch label keep their hue at lower brightness.
Their author, dates, parents, full message, and remote tags are available in the
graph/details panel. Tags use a remote prefix, such as `origin/v2.0`, and do not
introduce unrelated histories. Merge parents and clock-skewed commits retain
parent ordering. Incoming rows preserve your selected commit. At the top of the
graph, nearby new rows appear above it; elsewhere the viewport stays anchored.
Use Home to see the newest rows when there are more than fit above the selection.

Selecting a remote commit loads its changed-file list through the API. Open a file
(or press `d`) to load its contents and see the usual side-by-side diff. File lists
and contents are cached by immutable commit ID; revisiting them works offline.
This uses the [GitHub commit API](https://docs.github.com/en/rest/commits/commits)
and [GitLab commit diff API](https://docs.gitlab.com/api/commits/#get-the-diff-of-a-commit).
Remote requests run separately from local diffs. Provider file limits still apply;
remote submodules show an explanatory message. Marking arbitrary commit pairs
requires local objects. Previews and diffs do not fetch Git objects, move refs,
or touch the working tree. Checkout keeps the existing safety rules.

Immutable commit metadata and the assembled preview graph are cached separately
from the local graph. Branch tips and tags use a five-minute cache; forced refresh
checks them again. Cached commits appear before tag revalidation finishes. Failed requests
retain cached metadata and display an incomplete-preview status. Settings changes
and local-ref changes cancel outdated previews. Cache identities include the
project endpoint and resolved credential, including Git credential helpers. Tokens
are never written into these caches. `--no-cache` disables persistent preview and
diff caches too.

To bound work, previews allow 5,000 additional commits across remotes, 100 API
requests and 60 seconds plus the active request per remote, and 10,000 tags per
remote. Limits and unavailable ancestry are reported rather than inventing a
connection. The local graph remains usable during remote loading.

Checkout requires **Git for Windows on PATH**. It runs on a separate worker;
the browser and graph remain usable. Git's normal dirty-file, untracked-file,
in-progress-operation, and worktree protections apply. Git Rustle never uses
force checkout, reset, clean, or automatic stashing.

- A local branch is switched to directly.
- An existing local branch tracking the selected remote branch is reused,
  including a differently named local branch. This does not pull or reset it.
- Otherwise, a new local branch tracks the selected remote branch. If its
  displayed API tip is already in the object database, no fetch is needed.
  Missing objects, or a selection based only on local tracking refs, trigger
  an explicit fetch of that branch, without tags, submodules, or other refs.
- A conflicting local name is reported instead of overwritten. Select that
  local branch explicitly, or rename it with Git. Ambiguous tracking branches
  likewise require selecting the intended local branch.
- Single-branch clones gain a fetch mapping for the selected branch only.
  Bare repositories can be browsed but need a working tree for checkout.

While checkout is running, exit waits for you to retry after completion; this
prevents closing the application halfway through a worktree update. API discovery
does not delay exit. Git operations have a five-minute timeout and API requests
have a twelve-second timeout; failures remain visible in the browser.

#### Providers and authentication

HTTPS, SSH and SCP-style remote URLs on `github.com` and `gitlab.com` are
recognized automatically, including GitLab subgroups. Public repositories need
no token. For private repositories or higher API limits, set `GH_TOKEN`
(or `GITHUB_TOKEN`) for GitHub, or `GITLAB_TOKEN` (or `GLAB_TOKEN`) for GitLab.
GitHub tokens need repository **Contents: read**; GitLab tokens need API read
access. See the [GitHub branch API](https://docs.github.com/en/rest/branches/branches)
and [GitLab branch API](https://docs.gitlab.com/api/branches/).

When anonymous access fails, Git Rustle also tries the configured Git credential
helper for the repository's HTTPS host/path. Prompts are disabled inside the TUI;
sign in using Git outside the app if authentication is needed. Fetch uses Git's
own HTTPS/SSH authentication, independently of API environment tokens.

Press **`s`** in the graph or diff view to open the settings popup. **`F2`**
opens it from the branch browser without interfering with branch search.
The popup has **GitLab** and **GitHub** tabs. Select the remote, enter the
HTTPS server/API URL and project path, then test or save:

| Key | Action |
| --- | --- |
| `F1` / `F2`, or `Ctrl+Left/Right` | GitLab / GitHub tab |
| `Tab` / `Shift+Tab`, or Up/Down | Move between fields and buttons |
| Left/Right on the remote selector | Choose a repository remote |
| `Ctrl+T` | Test the current form without saving or fetching |
| `Ctrl+S` | Save the active tab's selected remote and refresh branches |
| `Ctrl+U` | Clear the focused field |
| `Ctrl+D` in the token field | Toggle removal of the saved API token |
| `Esc` | Close and discard unsaved edits (waits during saving) |

Tabs, fields, and buttons also support mouse clicks. The popup needs a terminal
of at least 60 columns by 25 rows. Each remote has one active provider; saving
one tab replaces that remote's provider configuration.

For **on-premise GitLab**, choose the GitLab tab and enter, for example,
`https://gitlab.company.example:8443/internal` and `team/subgroup/project`.
Git Rustle appends `/api/v4` if needed. GitHub Enterprise similarly uses
`/api/v3`. The API address can differ from the SSH clone host, so SSH aliases,
custom ports, and installation prefixes work without changing Git's clone or
push URL. Settings are saved in repository-local Git configuration (the
worktree config when enabled), equivalent to:

```powershell
git config remote.origin.rustle-provider gitlab
git config remote.origin.rustle-api-url https://gitlab.company.example/api/v4
git config remote.origin.rustle-project team/subgroup/project
```

Tokens typed in the form are masked and saved in **Windows Credential Manager**,
scoped to the exact project API endpoint for the current Windows user. They
never enter Git configuration or command-line arguments. Leaving the field
blank keeps the saved token; otherwise authentication falls back to cloud
environment tokens and then the Git credential helper. Removing a saved token
does not disable those fallbacks. Fetch/checkout still uses Git's own credentials.

Loading, saving, and connection tests run on background workers. Opening settings
adds no work to the initial local frame. Automatic remote previews start afterward. Test checks the
first API page and reports authentication, DNS, timeout, proxy, or TLS errors in
the popup; closing it during a test returns immediately. Successful saves clear
old in-memory remote results and start fresh discovery. There is currently no
persistent remote error log; failures appear in the branch browser or Test result.

Cloud environment tokens are never sent to custom hosts. HTTPS uses Windows
Schannel and the Windows certificate store, including corporate certificates
trusted there. API redirects are reported rather than followed with credentials.

#### Caching and offline behavior

Branch lists have a five-minute TTL. Cached results appear while stale data is
revalidated; `F5` forces revalidation. Every page is conditionally requested
with its ETag when available, and only complete successful lists replace the
disk cache. Network failures and rate limits retain the previous list. Different
repositories, remote URLs, API endpoints, and resolved credentials have separate
cache entries. Credential-helper-authenticated lists are persisted in their own
credential partition, never the anonymous cache. No tokens are written into
cache files. Cache entries are under `%LOCALAPPDATA%\git-rustle\cache\remotes`.

Branch listings query at most four remotes concurrently; commit previews use a
separate single worker. Listings are bounded to 100,000
branches, 60 seconds plus the active request, and 8 MiB per page. Unsupported
providers still expose local tracking refs and support Git-based checkout.
`--no-cache` disables both graph and remote disk caches; in-session branch lists
remain reusable.

### Performance benchmarks

Start with the [benchmark workflow for agents](docs/benchmarking.md): baseline
preservation, reproducible commands, regression analysis, and reporting conventions.
The [connection-dimming report](docs/connection-dimming-benchmarks.md) includes
the latest graph-styling measurements and startup comparison.

See the [remote UX and caching benchmark report](docs/remote-ux-benchmarks.md)
for the latest corrections.

See the [ghost-commit benchmark report](docs/ghost-benchmarks.md) for missing-commit
previews and background graph layout.

See the [settings benchmark report](docs/settings-benchmarks.md) for the popup,
on-premise setup validation, and startup comparison.

See [the remote-support benchmark report](docs/remote-benchmarks.md) for measured
startup and branch-browser results on the 85,593-commit Git repository. Preserve
a baseline release executable before editing, then compare release builds with:

```powershell
.\scripts\benchmark.ps1 -Repository "$env:TEMP\gr-repos\git" `
  -Baseline .\target\bench\baseline.exe -Samples 15
```

The script alternates the two executables, warms each mode once, and records
first-frame and full-walk timings from hidden native consoles. Raw samples and
median/p95 summaries are written under `target/bench/comparison`. Uncached means
Git Rustle's layout cache is disabled; it does not flush the Windows filesystem
cache. Do not build or run other benchmarks concurrently with the comparison.

## How it stays fast

- Startup runs the cache read, repository open and console setup in parallel;
  the first frame is painted from whichever is ready first.
- A stale cache is shown immediately while a background walk refreshes it;
  the fresh graph is swapped in when complete, preserving the selection.
- The commit walk streams batches into the UI, so even an uncached large
  repository shows its first screen in tens of milliseconds.
- Tree diffs only descend into subtrees whose ids differ; per-file line counts
  are computed on a separate thread and stream into the tree in chunks, so a
  5,000-file diff never delays the file you are looking at.
- Rendering diffs against the previous frame and wraps each flush in
  synchronized output (`DECSET 2026`), so Windows Terminal never shows a
  partially drawn screen.

## Debugging

- `GIT_RUSTLE_TIMING=1` prints startup timing marks to stderr on exit.
- `GIT_RUSTLE_AUTOQUIT=<ms>` quits automatically after the first frame.
- `GIT_RUSTLE_DUMP=<file>` writes the last rendered frame as text on exit.
- `GIT_RUSTLE_KEYS=<script>` replays key presses after the first frame, e.g.
  `"<space><down><space>d"`; named keys go in angle brackets.
