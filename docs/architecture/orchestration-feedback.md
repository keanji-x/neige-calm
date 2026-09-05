# Orchestration feedback (#1492)

Outcome: a task declaration cannot masquerade as a successful execution, and
Planner-authored gates receive the actual execution contract before worker work.
This change builds on PR #1490; it does not merge or redeploy that PR.

The report consumes the same diagnostics and identity join as the task inventory.
Execution status supersedes declaration readiness. Without execution diagnostics,
the report explicitly labels declaration readiness in a neutral color. Pending
and failure explanations remain on hover. Existing inventory navigation, compact
rows, summaries and hover explanations are already implemented and are not bugs.

Gate admission will reject obvious direct calls to the credential-dependent
`neige` CLI on new task writes. This is an authoring check, not a shell sandbox:
scripts, wrappers, aliases and dynamically constructed commands cannot all be
statically diagnosed. Local `neige --help` / `--version` remain usable. The
credential allowlist is unchanged. Existing stored declarations and attempts are
not migrated or rewritten.

Acceptance uses the real report renderer/router, report block write entry points,
and prompt renderer. Verify the report across execution states and query refresh,
preserve expanded details, test gate rejection before any report/task write, and
check artifact-based commands still pass. Independently review the full diff in
two isolated worktrees and verify the critical admission assertion by mutation.

Long-run checkpoint/resume, artifact handoff and old-attempt folding require
separate observed evidence. This change does not establish those capabilities or
change an ongoing dogfood run/deployment.
