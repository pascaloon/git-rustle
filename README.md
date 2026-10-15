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

## Usage

```
git-rustle [OPTIONS] [PATH]

  PATH          Repository (or any directory inside it); defaults to the cwd
  --no-cache    Ignore and do not write the layout cache
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
| `Tab`, `→` / `←`, `l` / `h` | Switch focus between graph and details |
| `[` / `]` | Shrink / grow the details panel |
| `Space` | Mark / unmark the selected commit (at most two) |
| `d` | Diff: the two marked commits, or the marked/selected commit against its parent |
| `Del` | Clear marks |
| `r` | Refresh (re-walk the repository) |
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
