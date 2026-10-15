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
| `r` | Refresh (re-walk the repository) |
| `q`, `Esc`, `Ctrl+C` | Quit |

### Mouse

Click a row to select it, scroll the wheel over either panel, and drag the
splitter between the panels to resize them.

## How it stays fast

- Startup runs the cache read, repository open and console setup in parallel;
  the first frame is painted from whichever is ready first.
- A stale cache is shown immediately while a background walk refreshes it;
  the fresh graph is swapped in when complete, preserving the selection.
- The commit walk streams batches into the UI, so even an uncached large
  repository shows its first screen in tens of milliseconds.
- Rendering diffs against the previous frame and wraps each flush in
  synchronized output (`DECSET 2026`), so Windows Terminal never shows a
  partially drawn screen.

## Debugging

- `GIT_RUSTLE_TIMING=1` prints startup timing marks to stderr on exit.
- `GIT_RUSTLE_AUTOQUIT=<ms>` quits automatically after the first frame.
- `GIT_RUSTLE_DUMP=<file>` writes the last rendered frame as text on exit.
