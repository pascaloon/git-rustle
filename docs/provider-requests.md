# Pull requests, merge requests and author photos

Open **PR / MR** in the graph toolbar, choose **Create PR / MR** for the selected
branch in Branches, or use a local branch's graph context menu. The composer opens
in its own tab. Switching back to Graph, Diff, Branches or Settings keeps the
draft, description and selected people. Closing the composer tab discards it.

Select the project, check source and target, and enter a title. Push the source
branch first with the existing Push action. This composer creates requests between
branches in the selected project; cross-project/fork requests are not yet supported.
The target starts with the project's default branch. Project selection clears
people and the target so identities cannot leak between projects.

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

Graph and commit-detail photos start with initials. After selection and the visible
graph viewport settle for 350 ms, a worker resolves visible authors, coalesces equal
commit email identities and loads small images. Each batch performs at most four
new author lookups. Scrolling doesn't start new lookups until the viewport settles.
Images and identities have one-day disk caches, partitioned by credentials; the UI
retains bounded session caches (512 commit entries, 256 author profiles). Settings
changes invalidate the session images. Downloads are bounded to 256 KiB and never
forward provider credentials to image hosts or follow redirects. Unavailable,
private, unsupported or unassociated profiles keep initials. `--no-cache` disables
these disk reads/writes as well as the existing application caches.

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
