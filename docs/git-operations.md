# Git operations and recovery

Use **Pull**, **Push**, and **New branch** in the graph toolbar. New branches start
at the current HEAD without changing checkout. Right-click a commit to merge it
into the current branch, rebase onto it, or prepare an interactive rebase. The
review names both the selected commit (including branch/tag labels) and the
current branch. Changing the current branch or HEAD invalidates that review.

Checking out a ghost branch in the graph offers **Fetch & checkout branch**.
It fetches only that remote branch and checks out its local tracking branch,
creating it if needed or updating the existing tracker by fast-forward only. Divergent local
commits are never reset or rebased, and Git refuses changes that would overwrite
working files. Ordinary local branch checkout keeps its existing offline behavior.

Right-click one local commit and choose **Create tag…** to name that exact commit.
An empty annotation creates a lightweight tag; adding a message creates an unsigned
annotated tag with your Git identity. Existing tags are never replaced. Tags stay
local until you publish them with Git. Creating a tag preserves HEAD, staging and
working files, and supports Undo/Redo. Missing remote-preview commits must be
fetched before tagging. The selected full commit ID remains fixed while the dialog
is open, even if the current branch advances.

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

## Stashes

Open the **Stashes** tab to list saved entries. The list loads on request;
Refresh picks up changes made in another Git client. **Apply…** requires a clean
working tree, restores the selected stash and keeps its saved entry. The selected
object stays fixed even if another stash is pushed before confirmation.
**Delete…** removes that selected saved entry after confirmation. Apply and Delete
support History recovery, including the stash stack and its reflog.

A conflicting apply opens Conflicts. Resolve and stage files, then choose
**Finish applying stash**; this does not create a commit or remove the stash.
**Abort…** discards tracked resolution changes using Git's merge reset and keeps
untracked files already restored by the stash. The resulting retained files are
recorded in History. A stash conflict discovered from another client has no Rustle
apply checkpoint; resolve and stage it without Continue or Abort controls.

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

## External diff and merge applications

In Settings → General, choose **Configure tools…** to edit the saved tool list.
Select an entry to change its name, application path, diff arguments and merge
arguments. Add or remove entries, then **Save tools** to persist the reviewed
list. The Diff view's external-tool picker lets you choose a saved application
by name for that comparison.

After the initial application load, a background scan discovers both VS Code and
Araxis once. The scan-completed flag is saved even when neither is installed;
subsequent launches load the saved list without repeating discovery. Existing
separate diff/merge configuration is migrated without discarding custom applications.
Custom applications use an absolute `.exe` path and one argument per line. An
argument containing spaces stays one argument. Scripts and shell command strings
are not executed.

Diff arguments use `{left}` and `{right}`. Merge arguments also use `{base}` and
`{output}`; left means ours, right means theirs, and output is the conflicted
working file. Include the application's wait option. Detected VS Code uses its
CLI wait launcher, and detected Araxis uses `/wait`, so temporary input copies
remain available until the comparison window closes. Custom applications must
provide equivalent waiting behavior.

External diff receives exact local or provider file bytes, including binary data,
up to 8 MiB per side. Merge receives the three Git index stages; a missing stage
is an empty input. It edits the working file without staging it. Save and close
the editor, review its output, then mark the edited file resolved. Other Rustle
repository mutations are blocked while that merge editor runs. Failed launches
retain input copies and a pre-merge file copy at the reported temporary path.

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
from garbage collection and is excluded from Rustle's graph. Created tag objects
are retained separately under `refs/rustle/tag-checkpoints/` so redo still works
after undo and garbage collection. Recovery objects are
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
settings/credentials, reflogs other than the stash stack, and server state are outside undo. External pushes
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

The focused [`benchmark-tools-list.ps1`](../scripts/benchmark-tools-list.ps1)
bundle measures shared startup, ordinary Diff delivery and General tab/manual
controls against freshly preserved application/test baselines. It measures the
new first-scan/list/editor/picker path in marked per-launch fixtures and compares
the existing controlled executable-launch worker; the new list component reports
first initialization, persisted reload, saving, migration and stable-ID selection.
Discovery fixtures contain fake VS Code/Araxis installations that are never
executed; worker launch measurements use a controlled test executable. See the
[benchmark workflow](benchmarking.md#saved-external-tool-list) for boundaries and
limitations. Earlier broad-suite results do not replace these measurements.

For tag creation, the focused
[`benchmark-tags-toolbar.ps1`](../scripts/benchmark-tags-toolbar.ps1) bundle runs
fresh native dialog/submit-to-tag-label measurements in disposable repositories,
the candidate `benchmark_tags` create/undo/redo harness for both tag types, and
shared desktop mutation plus large-worktree journal comparisons against preserved
baseline test executables. See the [main workflow](benchmarking.md#gpui-desktop-current-implementation)
for its exact invocation and timing boundaries. It reuses an existing large local
mirror for startup/search/scroll comparisons while recording new samples; it
never creates tags there. Preserve app and test baselines before edits, use fast
builds only for functional smoke, and finish final release validation/builds before
the sequential measurements. Physical typing, Git hooks in real projects, public
provider latency and display scanout require separate evidence.

Use the main [benchmark workflow](benchmarking.md). Preserve the release baseline
before implementation. Compare native startup and scrolling against that binary,
then measure mutation workers and UI delivery separately:

```powershell
cargo test --release git::operations::tests::benchmark_operations -- --ignored --exact --nocapture
cargo test --release git::operations::tests::benchmark_tags -- --ignored --exact --nocapture
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

## Branch cleanup and storage

**Cleanup** verifies configured fetch servers in a background worker and groups
references in three independent trees. **Fetched only** contains cached remote
refs whose cached tip exactly matches the verified server tip, with no
corresponding local branch. These entries are selected by default. **Matching**
contains local branches whose tip exactly matches the
verified server tip, including when its cached remote ref is stale or missing.
Deleting a selection removes the local branch and its cached remote ref when
present. **Old local** contains local branches with no server
counterpart whose last commit is at least one year old. This date is not a
branch creation date or the last time it was checked out.

Each tree supports folder checkboxes, mixed selection, and Select all/Unselect
all. Current branches in any worktree are excluded. Main, master and known remote
HEAD/default branch names are protected: their checkboxes are disabled and they
cannot be selected. Review the exact local references before deleting; the
worker rechecks their frozen object IDs. Failed server verification does not authorize
cleanup based on cached information. Verification uses read-only server
advertisements, with a five-second limit per remote and a twenty-second total
budget; it does not fetch objects or update cached refs. Offline or unverified
remotes exclude affected entries and produce warnings. Cleanup removes only
local references; it never deletes server branches. Undo restores deleted local and cached remote
refs, and recovery retains their objects.

**Optimize storage** repacks redundant object copies, retaining unreachable
objects and recovery history. It does not force-expire reflogs or immediately
prune unique commits. The result reports Git's object-storage KiB before and
after, including an increase when applicable; savings are not guaranteed.
