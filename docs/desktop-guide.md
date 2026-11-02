# Using Git Rustle

## Browse and compare commits

Pass a repository or any directory inside it on the command line. Use
`--no-cache` to skip persistent cache reads and writes for that session. Outside
a repository, the welcome screen lists recent projects and offers a folder picker.

A clean repository opens at HEAD. If you have uncommitted changes, the graph
selects **Pending changes**. Click **HEAD** to return to the checked-out commit.

Select a commit to see its message, author, parents, and changed files.
Double-click it for a side-by-side diff against its first parent. Ctrl+click
to select more commits and expand their details. With exactly two selected,
right-click to compare them. **Diff against branch base** shows what changed
since the selected commit and the base branch last shared an ancestor.

In **Diff**, pick a file in the tree. Drag to copy text from either side, or
double-click to select a line. Word wrap changes the display only. Switching
tabs keeps your place. Opening another comparison replaces the current diff.
See [syntax highlighting](syntax-highlighting.md) for supported formats and limits.

## Find a commit, branch, or line

Press Ctrl+F to search the current view. In the graph, an ordinary query searches
hashes, commit messages, authors, branch labels, and tags. Matching ignores case.
For a multiword query, all terms must match the same field, in any order.

| Example | Finds commits by |
| --- | --- |
| `f: Cargo.toml` | Changed path, including either name of a renamed file |
| `a: Morgan` | Author |
| `m: fix toolbar` | Words in the full commit message |
| `s: dd4d723` | Commit hash |

In **Branches**, search by branch name or latest commit author. In **Diff**,
search filenames or text from either version. Binary and unavailable files,
files over 8 MiB per side, and content beyond the 64 MiB index budget can only
match by filename. Escape clears the filter. In Diff, press Escape again to
return to the graph.

## Commit, discard, or stash changes

**Pending changes** lists staged, unstaged, and untracked files. Check whole
files to commit their current working contents, including any unstaged edits.
Unchecked files retain their index entries. Enter a commit message and, if you
want to commit on a new branch, a new branch name. Your draft survives opening
a diff or a failed commit.

To discard one file, right-click it, choose **Discard file**, then click the
confirmation. This restores both the index and working copy to HEAD, or removes
a new file. Undo can recover it. To set the whole change aside instead,
right-click **Pending changes** and choose **Stash changes…**. See
[stashes and recovery](git-operations.md#stashes).

## Browse remote branches

Double-click a branch in **Branches** to locate its tip or preview its remote
history. GitHub and GitLab previews use read-only requests and leave your local
refs and working tree alone. If the server has moved since your last fetch,
a cloud badge marks its current tip and the ordinary remote badge marks your
locally fetched tip.

Press F5 to check local changes and the server tips for branches in the graph.
Ctrl+F5 also reloads the full branch and tag lists, provider details, and
stashes. Right-click a commit with a PR or MR badge to open its request online.

For a self-hosted project, select its Git remote, provider, and project path
under **Settings → Project**. Set the API URL and token under **GitHub** or
**GitLab**. These provider settings apply across projects for the Windows user.
Tokens are stored per API installation in Windows Credential Manager.

## Use AI tools

Install Codex CLI or Claude Code CLI on your PATH and sign in from a terminal.
In **Settings → AI**, choose the CLI and optionally set a model or edit the
prompts. The provider receives the source text included in each request.

| Action | What it does |
| --- | --- |
| **Auto-fill** | Drafts an editable commit message from checked files, including untracked files. |
| **Summarize all files with AI** | Summarizes diff hunks for the whole comparison, including filtered or unchecked files. Binary files contribute metadata. |
| **Resolve with AI** | Drafts a resolution for one conflicted text file. Review it before choosing **Save and mark resolved**. |
| **Merge into current branch with AI** or **Rebase current branch onto this with AI** | Starts the Git operation immediately, saves and stages AI resolutions, and continues automatically. Cancellation or failure leaves an in-progress conflict available in **Conflicts**. |

Use **Cancel AI** in the status bar to stop a request. Each CLI request times
out after five minutes. Summaries are subject to the model’s context limit.
Other requests have a 256 KiB input limit. Completed AI merges and rebases
appear in undo history.

## Update the app

Git Rustle checks for stable releases at startup. To check now, open
**Settings → General → Check for updates**. Choose **Update and restart** to
install an available version. The app verifies the download’s SHA-256 digest and
reopens the repository with your pending commit draft and file selections. Save
other forms first. If the executable directory is read-only, download the new
release manually.
