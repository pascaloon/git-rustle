# Git operations and recovery

Use **Pull**, **Push**, and **New branch** in the graph toolbar. New branches start
at the current HEAD without changing checkout. Right-click a commit to merge it
into the current branch, rebase onto it, or prepare an interactive rebase. The
review names both the selected commit (including branch/tag labels) and the
current branch. Changing the current branch or HEAD invalidates that review.

Commits carrying local branch labels also offer **Rename <branch>** and
**Delete <branch>**. Each branch has its own menu entry. Delete uses Git's normal
merged-work checks; there is no ambiguous "delete unmerged" action. The current
branch cannot be deleted. Remote labels and tags do not get local branch actions.

**Push** always reviews the current branch and automatically resolves its push
remote (`branch.<name>.pushRemote`, `remote.pushDefault`, upstream remote, then
`origin`). It uses the configured upstream branch when publishing to that remote;
otherwise it uses the current branch name. Only that branch is published.
Multiple push URLs are refused because one shield cannot describe several servers.
The worker checks the actual push URL with `ls-remote`, without fetching or
silently changing the last-fetched lease:

- **Green shield:** a new branch or normal fast-forward push; remote history is preserved.
- **Orange shield:** history diverged, but the server still matches the fetched
  tip. The confirmation explicitly offers **Force push with lease** and warns
  that remote-only commits will leave that branch's history.
- **Red shield:** the fetched lease is stale, or the server cannot be verified.
  Push is blocked. The dialog explains the potential loss of other people's work;
  fetch and review changes before retrying. No unprotected force fallback exists.

The reviewed local commit, destination and lease are frozen. Switching branches,
committing or changing the destination requires a new review. A remote change
after review is still checked by Git on push; a background fetch cannot weaken the
approved lease. Green means the history update is safe, not that authentication,
hooks or server branch rules will accept it. A dispatched push seals earlier
recovery history even on timeout; merely opening or canceling review does not.
Publishing a new branch also configures its upstream when none exists.

Pull fetches only the configured upstream, without tags or submodules, then merges.
Merge, pull and rebase require an attached branch and a clean working tree. Git's
own hooks and authentication remain in effect; prompts cannot block the UI.

## Interactive rebase

Right-click the intended base commit and choose **Interactive rebase from here…**.
The confirmation prepares a plan before changing Git history. The **Interactive
rebase** tab exists only while that plan is open or an interactive rebase is active.
Discarding a plan, completing or aborting the rebase removes the tab. The oldest
commit appears first; the selected base itself is excluded. Move complete lines to reorder; use `pick`, `reword`, `squash`,
`fixup`, `drop`, or `edit`. Every original full ID must appear exactly once.
For `reword`, replace the subject after the ID with the new message. Squash keeps
the combined messages; fixup keeps the preceding message. Edit pauses for manual
amendment in Git, then Continue resumes the sequence. Arbitrary shell commands,
duplicate IDs and a squash/fixup without a preceding commit are rejected.

The loaded branch, HEAD and base are frozen. If the branch or HEAD changes, prepare the plan again. Linear
ranges are supported; plans containing merge commits are rejected rather than
silently flattening their topology. Ordinary Git rebase retains Git's configured
behavior except that automatic stashing and updating other branches are disabled.

## Conflict resolution

**Conflicts** opens automatically when an operation pauses, including an operation
discovered when opening a repository. Each unresolved file offers whole-side
selection or comparison and editing for UTF-8 text up to 2 MiB. Saving checks that
the file has not changed since it was loaded. Binary, symlink, large, rename and
delete conflicts can use whole-side selection or an external editor, followed by
Mark resolved. Whole-side choices replace and stage the file; an absent side means
deletion. Marker checks catch unfinished text resolutions.

During rebase, “ours” means the new base plus already replayed commits and “theirs”
means the commit currently being replayed. Continue refuses unresolved paths.
Skip omits the current replayed commit; Abort returns to Git's starting state and
discards conflict-resolution edits. A whole sequence occupies one history entry.

## Undo and redo

The bottom-right **History** button opens a compact timeline, newest first.
**Current** marks your position; faded **Redo** steps are ahead of it.
Use Undo/Redo or double-click a state to restore it. There are no Ctrl+Z/Ctrl+Y Git shortcuts.
Creating another operation after undo replaces the redo tail.

**Clear… → Clear history** removes all undo and redo steps for this worktree,
including after restarting the app. It preserves the current files, staging,
commits, branches and configuration. Clearing is unavailable during an active Git
operation. Recovery objects remain protected; this clears navigation history,
not checkpoint storage shared with other worktrees.

History is per worktree and persists in its Git directory under
`rustle-operations/journal.json`. Checkpoints include HEAD, refs, local config,
the index, and Git trees for staged and working files (including untracked files,
excluding ignored files). `refs/rustle/checkpoints/` protects checkpoint objects
from garbage collection and is excluded from Rustle's graph. Recovery objects are
retained even when history is invalidated; the visible list retains 100 operations.
Checkpoint storage can grow with edited files. Do not delete it while recovery
is needed. Normal branch pushes do not publish recovery refs; `git push --mirror`
outside Rustle would publish all refs, including recovery refs.

Every restore verifies the expected refs, HEAD, config, staging and working-tree
content. Outside edits, commits, configuration/ref changes and push attempts
invalidate earlier navigation. A branch held by another worktree cannot be moved.
The real index is locked during restoration, reference writes use expected-old
values, and a two-tree checkout refuses collisions instead of using reset --hard.
These Git workflows share a repository-wide OS lock. Git operations outside
Rustle still require normal coordination; no application can make independent
editors or arbitrary hooks participate in one transaction.

Git-tree snapshots use configured clean/smudge filters and EOL rules; they are not
byte-for-byte filesystem backups. Ignored files, Git hook side effects, provider
settings/credentials, reflogs and server state are outside undo. External pushes
that leave no local state change cannot be detected. Checkpointed mutations in
submodule and sparse-checkout worktrees are refused until their recovery semantics
can be guaranteed. Files
marked assume-unchanged or skip-worktree also block checkpointed operations,
because Git status can hide their local edits. Clear those flags using Git first.
Recovery
requires a writable Git directory and a Git installation supporting the commands
used by the application. If an OS/filesystem failure interrupts a multi-file
restore, stop other mutations and inspect the retained journal/checkpoint refs;
HEAD, refs, config and working files are not a single filesystem transaction.

## Performance validation

Use the main [benchmark workflow](benchmarking.md). Preserve the release baseline
before implementation. Compare native startup and scrolling against that binary,
then measure mutation workers and UI delivery separately:

```powershell
cargo test --release git::operations::tests::benchmark_operations -- --ignored --exact --nocapture
cargo test --release git::operations::tests::benchmark_large_worktree_operations -- --ignored --exact --nocapture
cargo test --release engine::tests::benchmark_desktop_mutations -- --ignored --exact --nocapture
cargo test --release engine::tests::benchmark_desktop_push_refresh -- --ignored --exact --nocapture
./scripts/benchmark-operations.ps1 -Samples 15 -OutputDirectory target/bench/<run>/operations-native
```

The mutation harness uses disposable repositories and includes checkpoint/journal
cost. The native suite measures action dispatch through the next rendered frame
for the new-branch dialog, branch creation, rebase plan and history popup. Both have
one discarded warm-up and 15 retained samples. They do not establish live remote
latency, large binary-file snapshot latency or physical input/display latency.
The push-to-graph harness publishes to a disposable local bare remote after
installing stale API branch metadata. It asserts a forced discovery request and
the updated graph label, and records push result delivery and time through graph
model refresh separately. Its provider is unconfigured, so discovery falls back
to local tracking refs; it does not measure live provider API latency or painting.
The large-worktree harness separately measures branch creation and history
navigation with 1,001 tracked files, both clean and with 100 staged/unstaged files
plus an untracked file, checking that staging and working contents survive.


### Contextual action measurements

`benchmark-operations.ps1` accepts `-Scenarios` for focused native runs. Use
`operations` (new-branch dialog), `graph-menu`, `integrate-review`, `rebase-plan`,
`push-review`, `push-review-orange`, and `push-review-red`. The push fixtures use
local bare remotes and check the resulting risk state; they do not publish anything.
Older executables only implement the original operations/rebase/history scenarios.
Compare graph scrolling and startup with the preserved release too. These are
action-handler-to-frame timings, not physical pointer or network latency.
