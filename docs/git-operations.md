# Git operations and recovery

## Branches, checkout, and tags

**New branch** in the graph toolbar creates a local branch at HEAD and switches
to it. Pending changes carry over when safe. If switching would overwrite a
file, Git refuses the operation. Undo restores both the branch and the checkout.

To check out a remote branch, choose **Detached HEAD** to inspect its tip or
**Pull branch** to work on a local tracking branch. Pull branch fetches that
branch, creates its local tracker if needed, and fast-forwards an existing
tracker only when possible.

Right-click a local commit for these actions. When several branches share the
commit, look in the **Branch** submenu for actions on a particular branch.

- **Create tag…** tags the selected commit. Leave the annotation blank for a
  lightweight tag, or enter a message for an unsigned annotated tag. Existing
  tags are protected. Publish the tag separately with Git.
- **Rename** or **Delete** acts on the named local branch. Deleting an unmerged
  branch requires an explicit checkbox. Branches checked out in any worktree
  are protected.
- **Reset current branch here…** moves your branch to the chosen commit.
  **Soft** keeps staging and working files. **Mixed** resets staging and keeps
  working files. **Hard** also resets tracked working files.

## Pull, merge, and push

**Pull** fetches the configured upstream and merges it. To merge a selected
commit into the current branch, or rebase the current branch onto it,
right-click the commit. Pull, merge, and rebase require an attached branch and
a clean working tree. Review the source and destination before confirming.

**Push** reviews one branch and its destination. The shield tells you what Git
Rustle found at the push URL.

| Shield | What you can do |
| --- | --- |
| Green | Publish a new branch or fast-forward the server branch. Server rules and hooks still apply. |
| Orange | The histories have diverged, but the server still matches the last fetched tip. **Force push with lease** replaces that history. Review the commits the branch will lose before confirming. |
| Red | The lease is stale or the destination could not be verified. Fetch and review before retrying. |

Git Rustle first checks the branch’s `pushRemote`, then `remote.pushDefault`,
then the upstream remote. It falls back to `origin`. It uses the upstream
branch name when pushing to that remote. Multiple push URLs require using Git
directly. The review fixes the local commit, destination, and lease so a
background fetch cannot weaken a force push. Sending a push ends earlier undo
history, even if the push times out. Canceling the review keeps that history.

## Stashes

Right-click **Pending changes** and choose **Stash changes…** to save staged,
unstaged, and untracked files. In **Stashes**, **Apply…** restores the selected
stash into a clean working tree and keeps the entry. **Delete…** removes it.
Both actions support Undo.

If applying causes conflicts, resolve and stage the files, then choose **Finish
applying stash**. The entry stays in the list. **Abort…** discards tracked
resolution edits and retains untracked files already restored. A conflicted
stash applied outside Git Rustle has no app checkpoint for Continue or Abort.

## Interactive rebase

Right-click the commit *before* the range to rewrite and choose **Interactive
rebase from here…**. The plan lists commits oldest first and excludes the base.
Use the arrows to change replay order and the selector beside each commit to
pick, reword, squash, fix up, drop, or edit it. Squash combines messages. Fixup
keeps the preceding message. Edit pauses for you to amend the commit in Git
before choosing **Continue**.

Review the plan before starting. You can discard it without changing history.
Plans require a linear range. If the branch or HEAD changes, prepare a fresh plan.

## Conflict resolution

When Git pauses, **Conflicts** shows the incoming and local commits. During a
rebase, incoming is the new base plus commits already replayed, and local is
the commit being replayed.

1. Select a conflicting file. For UTF-8 text up to 2 MiB, edit the result and
   choose **Save and mark resolved**. Drafts survive switching files. If another
   editor changed the file, **Discard draft & reload** loads its current contents.
2. To take a whole side, choose **Use incoming…** or **Use local…**. Confirming
   replaces and stages that file. An absent side means deletion. For a file you
   resolved externally, review it and choose **Mark file resolved**.
3. Choose **Continue** after every conflict is staged. Where available, **Skip
   commit…** omits the current commit. **Abort…** discards resolution edits.

### External diff and merge applications

Open **Settings → General → Configure tools…** to add or edit a tool. Git Rustle
can discover VS Code and Araxis. For a custom tool, enter an absolute `.exe`
path and one argument per line. Include the tool’s wait option so its temporary
inputs remain available until you close it.

Diff arguments use `{left}` and `{right}`. Merge arguments also accept `{base}`
and `{output}`. Left is Git’s *ours*, right is *theirs*, and output is the working
file. External diffs accept up to 8 MiB per side, including binary files. A
merge tool edits the working file. Close it, review the result, and mark the
file resolved in Git Rustle. A failed launch reports where it kept the input
copies and the pre-merge file.

## Branch cleanup and storage

**Cleanup** checks the fetch servers before offering local references for
deletion. Review the groups separately.

| Group | What deleting an entry removes |
| --- | --- |
| Fetched only | A remote-tracking ref with no corresponding local branch. The branch still exists on the server. Eligible entries start checked, even if their cached tip is stale. |
| Matching copies | A local branch whose tip matches the verified server tip, plus its cached remote-tracking ref when present. |
| Old local branches | A local branch with no server counterpart whose last commit is at least one year old. It may contain unique commits. |
| Missing on server | A remote-tracking ref whose server branch has disappeared. It may be the last copy and starts unchecked. |

Cleanup deletes only local references. Checked-out branches and protected default
branches are ineligible. If a server cannot be verified, affected entries are
excluded. You can also opt to remove local tags unique to the selected branches.
Undo restores deleted refs.

**Optimize storage** repacks objects while retaining recovery history and
unreachable commits. The result shows the actual before and after sizes.

## Undo and redo

Open undo history in the bottom-right status bar. Double-click a state to restore
it. Starting a new operation after Undo replaces the redo steps. **Clear history**
removes this worktree’s navigation history across restarts.

History keeps up to 100 operations per worktree. A checkpoint covers HEAD,
refs, local Git config, the index, and tracked and untracked working files.
Restoring verifies the expected state first. Changes made outside Git Rustle can
invalidate earlier steps. A branch held by another worktree cannot be moved.

Git filters and line-ending rules apply to checkpointed files. Ignored files,
hook side effects, provider credentials, most reflogs, and server state are
outside recovery. Checkpointed operations require a writable Git directory and
exclude submodule or sparse-checkout worktrees and files marked assume-unchanged
or skip-worktree.

The journal lives in the worktree’s Git directory under
`rustle-operations/journal.json`. Recovery objects stay under
`refs/rustle/checkpoints/` and `refs/rustle/tag-checkpoints/` even after history
is cleared. A `git push --mirror` includes these refs. If a filesystem failure
interrupts a restore, stop other mutations and inspect the journal and refs
before proceeding.

## Performance validation

Use the [benchmark workflow](benchmarking.md) to preserve release app and test
baselines. Measure Git worker time separately from native action-to-paint time.
The focused [operation runner](../scripts/benchmark-operations.ps1) covers
branch creation, rebase plans, history, and push reviews in disposable repos.
See also the [tag and toolbar runner](../scripts/benchmark-tags-toolbar.ps1)
and [external tools runner](../scripts/benchmark-tools-list.ps1).

```powershell
cargo test --release git::operations::tests::benchmark_operations -- --ignored --exact --nocapture
cargo test --release git::operations::tests::benchmark_tags -- --ignored --exact --nocapture
cargo test --release git::operations::tests::benchmark_large_worktree_operations -- --ignored --exact --nocapture
cargo test --release engine::tests::benchmark_desktop_mutations -- --ignored --exact --nocapture
cargo test --release engine::tests::benchmark_desktop_push_refresh -- --ignored --exact --nocapture
```
