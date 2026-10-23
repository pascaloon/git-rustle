# Pull requests, merge requests and author photos

Open **PR / MR** in the graph toolbar, choose **Create PR / MR** for the selected
branch in Branches, or right-click a local branch row in Graph and choose
**Create PR / MR from <branch>…**. This includes the checked-out branch. A row with
one local branch shows the action directly; when several branches share its commit,
the per-branch actions appear in the **Branch** submenu. The composer opens
in its own tab. Switching back to Graph, Diff, Branches or Settings keeps the
draft, description and selected people. Closing the composer tab discards it.

Select the project, check source and target, and enter a title. Push the source
branch first with the existing Push action. This composer creates requests between
branches in the selected project; cross-project/fork requests are not yet supported.
Source and target are searchable branch selectors. Choices come from local branches
and the selected remote's fetched branches; they exclude tags, symbolic aliases and
other remotes. A fetched tip takes precedence over a local branch with the same name.
Fetch first if a branch is missing or its history is out of date, then use
**Refresh branches** to update choices without losing an explicit target selection.

Targets exclude the source and are ordered by the nearest shared ancestor on the
source's first-parent history, then the distance from the target to that ancestor.
For example, a feature branched from Dev prefers Dev over Main, including when Dev
has advanced since the fork. Equal scores prefer the project's default branch,
then branch name. Descendants and unrelated or unavailable history follow known
bases. Walks are bounded; incomplete history is indicated beside the form.

Opening the form or changing the source/project selects the first ranked target.
An explicit target choice stays selected during member refreshes. Changing the
source resets that choice and ranks again. Project selection clears people and
the target so identities cannot leak between projects. Branch work runs separately
from provider member requests; filtering an open selector performs no I/O.

Assignees and reviewers are searchable by name or username, with keyboard
navigation and removable selections (up to ten per field). GitHub suggestions use
the project's available assignees, which works with PR read permissions and
supports public projects without collaborator-list access; GitLab includes inherited project members. Server
permissions and plan limits still determine which assignments are accepted.
Use **Refresh project members** after membership or token changes. Member lookup
failure leaves the form available, with a warning and retry control.

Descriptions are plain Markdown, edited with syntax highlighting. **Write** and
**Preview** share the same text. The preview is local and can differ from a
provider's extensions. **Create as draft** uses GitHub's draft flag and GitLab's
`Draft:` title convention.

Creation runs on a background worker. GitHub creates the PR and then assigns
people and requests reviews with its separate APIs. If either follow-up fails,
the success card keeps the created PR's link and explains what needs updating on
GitHub; it never submits a second PR. Draft PRs may reject review requests until
marked ready. Network/ambiguous creation failures disable resubmission and ask you
to check the provider first. Opening another repository or closing the application
waits for an active submission to finish.

Configure credentials in Settings. The token needs access to the project and
permission to create requests; membership and review APIs can need additional
permissions. No token is put in a draft, cache payload, status message or URL.

## Performance and caches

The composer allocates its inputs and member indexes when first opened. Provider
I/O and disk caches run on workers. Both people pickers share one immutable roster;
typing lowercases the query once and filters reusable indexes without API calls.
Complete member metadata is cached for five minutes and partitioned by repository,
remote endpoint and credentials. Failed/partial rosters are not cached as success.
Pagination is bounded to 10,000 entries and reports an error if incomplete.

Graph and commit-detail photos start with initials. Newly visible rows inspect only
their local Git author headers on a background worker, without waiting for scrolling
to settle or for a provider request to finish. An exact author email immediately
reuses the same in-memory image on every matching row, even when those commits have
never requested a photo. Equal display names alone do not share photos. This local
step does not decode commit messages or read Git on the UI thread.

New provider/photo requests still wait until selection and the visible graph viewport
settle for 350 ms. Each batch handles at most four unresolved commits. The UI retains
bounded session caches (512 commit entries/author bindings, 256 author profiles).
Settings changes invalidate session images. New rows still need a short local author
lookup; only known commit-to-email bindings can render their photo in the first frame.

Photos are stored once per avatar URL as the original compressed PNG/JPEG/WebP bytes
under the user's `git-rustle/cache/avatars-v1` directory (`LOCALAPPDATA` on Windows).
These binary files are shared across repositories, worktrees and credentials; image
downloads are public and never receive provider credentials or follow redirects.
An email-to-avatar URL binding is also shared across repositories of the same provider
API installation, preventing repeat commit API lookups after restarting. Different
providers/installations have separate author bindings. Both photos and bindings
expire after one day so changed profiles can refresh. Fresh older JSON image caches
migrate on use without downloading, and their migrated entry is removed. PR/member
metadata and commit API caches retain their existing credential partitions.

Downloads and disk image reads are bounded to 256 KiB. Unavailable, private,
unsupported or unassociated profiles keep initials. `--no-cache` disables all photo
and identity disk reads/writes as well as the existing application caches.

## Provider contracts

- [GitHub PR creation](https://docs.github.com/en/rest/pulls/pulls#create-a-pull-request),
  [assignees](https://docs.github.com/en/rest/issues/assignees#add-assignees-to-an-issue),
  [review requests](https://docs.github.com/en/rest/pulls/review-requests#request-reviewers-for-a-pull-request)
  and [collaborators](https://docs.github.com/en/rest/collaborators/collaborators#list-repository-collaborators).
- [GitLab MR creation](https://docs.gitlab.com/api/merge_requests/#create-mr),
  [members](https://docs.gitlab.com/api/project_members/) and
  [avatar resolution](https://docs.gitlab.com/api/avatar/).

See [the benchmark workflow](benchmarking.md#provider-composition-and-photos) for
reproducible measurements and the difference between fixture and live results.
