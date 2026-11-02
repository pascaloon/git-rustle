# Git Rustle

Git Rustle is a native Git client for Windows, built in Rust with GPUI. Use the
commit graph to trace a change, compare branches side by side, or preview a
GitHub or GitLab branch before fetching it.

[Git Rustle website](https://pascaloon.github.io/git-rustle/)

## Get started

[Download Git Rustle for Windows x64](https://github.com/pascaloon/git-rustle/releases/latest).
You need Windows 10 or 11 and a DirectX-capable graphics driver. Install Git for
Windows and put `git` on your PATH to commit, switch branches, and use recovery.

Run the downloaded executable and choose **Open Directory** from the welcome
screen, or pass a repository path in PowerShell. Running it inside a repository
opens that repository automatically.

```powershell
.\git-rustle-x86_64-pc-windows-msvc.exe C:\src\my-project
```

## Everyday use

- **Explore history.** Select a commit to read its message and see which files
  changed. Double-click to diff it against its first parent. To compare two
  commits, Ctrl+click the second and choose the comparison from the context menu.
- **Find a change.** Press Ctrl+F in the graph to search commits. Try
  `f: Cargo.toml` for commits that touched a file. In a diff, Ctrl+F searches
  filenames and text on both sides.
- **Make a commit.** Open **Pending changes**, check the files you want, write
  a message, and click **Commit**. Each checked file contributes its current
  working contents. Unchecked files keep their staged and unstaged changes.
- **Work with branches.** Open **Branches** to find a local or remote branch.
  Use the graph toolbar to create a branch, pull, or review a push. The status
  bar’s undo history lets you revisit recoverable local operations.

The [desktop guide](docs/desktop-guide.md) covers search, remote previews, and
optional AI tools. See [Git operations and recovery](docs/git-operations.md) for
conflicts, cleanup, and what Undo can restore. To open a request from a branch,
see [pull requests and merge requests](docs/provider-requests.md).

## Build from source

Install stable Rust with the Windows MSVC toolchain and Visual Studio C++ Build
Tools. From the repository root, run the following in PowerShell.

```powershell
cargo build --release --locked
.\target\release\git-rustle.exe .
```

Use the release executable when copying Git Rustle to another PC. For shorter
local builds, use `cargo build --profile fast --locked` and run
`.\target\fast\git-rustle.exe`. Both profiles embed the graphics shaders.

For development checks and release steps, see [development](docs/development.md).

## License

[MIT](LICENSE)
