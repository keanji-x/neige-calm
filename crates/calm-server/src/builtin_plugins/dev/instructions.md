## Development capability

The development plugin supports Git worktrees, validation and pull requests through the kernel's recorded operations. This guide does not enable the plugin or grant tool permissions. The kernel checks enablement, Track scope and role for every call.

Its tools are named `plugin.dev.neige.git-forge_<name>` (e.g. `plugin.dev.neige.git-forge_gh.pr.checks`); list them with `neige tools names --prefix plugin.dev.neige.git-forge_`.

To deliver an attached Git Track, use `neige.dev.publish`. It pushes a completed task's candidate and opens or reuses the PR. Report-only work does not need a PR.

When the PR conflicts or the upstream moved on, catch the Track up with a codex or claude task declared `start: "upstream"` with a gate (its worker replays your last done commit on the fetched upstream), then publish again: `neige.dev.publish` replaces the Track's own branch when it is absent or at a commit an attempt of this Track made, and exits 22 for anyone else's commit (ask the user). After a failed catch-up, continue with an ordinary task only when its worker left partial work; if it never ran, declare another `start: "upstream"` task.

The selected template determines when review and user ratification are required; read its working method and acceptance conditions.

Read issue requirements with `gh.issue.view`, and read the discussion with
`gh.issue.comments`. Pass a new `attempt` to either tool when refreshing; reuse
the attempt on retries.

Use `gh.issue.comment` to post a progress update, question, or result. Pass `repo`,
`issue`, Markdown `body`, and a stable `idem` for that logical comment. Reuse
the same idem and body on retries; use a new idem for another comment. The kernel
records and parks the write before running gh. A pending receipt is not proof
that the comment was posted: retry the same call to retrieve its outcome. A
hidden marker supports recovery without confusing an existing human comment
with this write.
