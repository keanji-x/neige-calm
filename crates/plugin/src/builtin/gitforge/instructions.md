## Development capability

The development plugin supports Git worktrees, validation and pull requests through the kernel's recorded operations. This guide does not enable the plugin or grant tool permissions. The kernel checks enablement, Track scope and role for every call.

Its tools are named `plugin_gitforge_<name>` (e.g. `plugin_gitforge_gh_pr_checks`); list them with `neige tool ls --prefix plugin_gitforge_`.

To deliver an attached Git Track, use `plugin_gitforge_publish`. It pushes a completed task's candidate and opens the PR, or gives the open PR the new title and body. Report-only work does not need a PR.

When the PR conflicts or the upstream moved on, catch the Track up with a codex or claude task declared `start: "upstream"` with a gate (its worker replays your last done commit on the fetched upstream), then publish again: `plugin_gitforge_publish` replaces the Track's own branch when it is absent or at a commit an attempt of this Track made, and exits 22 for anyone else's commit (ask the user). After a failed catch-up, continue with an ordinary task only when its worker left partial work; if it never ran, declare another `start: "upstream"` task.

The selected template determines when review and user ratification are required; read its working method and acceptance conditions.

Read issue requirements with `gh_issue_view`, and read the discussion with
`gh_issue_comments`. Pass a new `attempt` to either tool when refreshing; reuse
the attempt on retries. `gh_issue_view` returns JSON number/url/state/title/body/labels.

Use `gh_issue_search` with repo/query to find related issues, including closed ones
by default; state and limit (1..100, default 30) bound results. A new attempt
refreshes them. Search is not an atomic deduplication or recovery guarantee.
When issue creation is authorized, use `gh_issue_create` with repo/title/body and
a stable idem. Repo is owner/name or host/owner/name. Retrieve completion with
identical arguments from the same card; result.event contains issue_number and
issue_url. Changed content under the same idem is rejected. An Unknown failure
must be investigated rather than blindly retried with a new idem.

Use `gh_issue_comment` to post a progress update, question, or result. Pass `repo`,
`issue`, Markdown `body`, and a stable `idem` for that logical comment. Reuse
the same idem and body on retries; use a new idem for another comment. The kernel
records and parks the write before running gh. A pending receipt is not proof
that the comment was posted: retry the same call to retrieve its outcome. A
hidden marker supports recovery without confusing an existing human comment
with this write.
