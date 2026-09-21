# Fixed sentences for a failed Git delivery (#1727 S4 D2 code table). One line per key: `<key>` TAB `<sentence>`.
# Keys 10-15 are the delivery script's exit codes; `git` is any other non-zero exit; `no_result` is a failed
# forge action that left no result file; `workspace_missing` and `unresolved` are kernel classifications.
# `{code}` is substituted with the exit code. The settlement copies the sentence (plus the script's stdout
# evidence lines for 10/12/15) into `task_git_deliveries.failure_reason`; the Planner reads it from the row.
10	The lease directory is not the registered lease worktree: its realpath or its git common dir differs from what the lease row recorded (observation line below).
11	HEAD is not on the slice branch (the worker switched branches or detached HEAD); nothing was staged or committed.
12	The worktree resolves to the lease path and repository but git does not list it as a registered worktree (observation line below); nothing was staged or committed.
13	The lease base commit could not be read in this repository, or the ancestry observation (merge-base) failed; no candidate ref was pinned (a commit the script may have made before that observation stays on the branch tip).
14	A git observation (worktree provenance, branch or index) failed before the script could decide anything; nothing was staged, committed or pinned.
15	The worktree has an operation in progress (a merge, cherry-pick, revert, rebase or am, or unmerged index entries — evidence below: the unmerged paths, or the pseudo-ref or state directory git left, e.g. `REBASE_HEAD` / `rebase-apply`); nothing was staged or committed. Finish or abort it first.
git	git exited with status {code} before a candidate ref was pinned (a hook, index.lock, disk or permission failure); no candidate exists for this delivery.
no_result	The delivery action ended without a result file and the probe found no candidate ref; no candidate exists for this delivery.
workspace_missing	The lease directory no longer exists; this delivery cannot be retried. Abandon it or declare a new task.
unresolved	The kernel cannot prove what happened to this delivery (an infrastructure failure, a timeout or an unknown probe verdict). A retry runs the script's own checks again.
