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

## Gate authoring contract

Each step runs under `/bin/sh`. The initial environment contains only inherited
`PATH`, `HOME`, `LANG`, `LC_ALL`, `TERM` when present, configured HTTP/HTTPS proxy
settings (upper and lower case), and the verifier's internal exit-file path.
MCP socket/token and incidental server credentials are absent. Gates cannot use
`neige cat`, `neige state` or task reporting; the Planner can use those commands
outside the gate to inspect `plan/<key>/output` and `plan/<key>/gate.log`.

Directory precedence is explicit `gate.cwd`, then the bound worker execution's
durable checkout. Declaration cwd and Track cwd apply only when no execution is
bound. A missing bound checkout is a verification infrastructure failure; it does
not authorize fallback to a different checkout. PR #1490 owns that behavior.

Have workers write durable files at specified paths in their checkout and record
artifact paths/hashes with their exact task identity. For example:

```json
{"steps":[{"name":"semantic tests","cmd":"python3 -m unittest discover"}]}
```

`test -s artifacts/result.json` is useful only as an existence/nonempty check;
it does not establish correctness. Prefer semantic tests with independently
specified expected results. Gates can run again after restart, so checks must be
re-runnable. Downstream workers have separate checkouts: give them the producing
checkout/path and expected hash explicitly rather than assuming relative paths
refer to shared files. A worker-reported hash is a claim to verify, not a kernel
attestation.

The kernel's current failure classes distinguish worker report/timeout/spawn
failures from `gate-red` (verification rejected the result), `gate-infra`
(verification infrastructure) and `gate-timeout`. The report retains those exact
causes on hover rather than inventing a second failure classification.

Acceptance uses the real report renderer/router, report block write entry points,
and prompt renderer. Verify the report across execution states and query refresh,
preserve expanded details, test gate rejection before any report/task write, and
check artifact-based commands still pass. Independently review the full diff in
two isolated worktrees and verify the critical admission assertion by mutation.

Long-run checkpoint/resume, artifact handoff and old-attempt folding require
separate observed evidence. This change does not establish those capabilities or
change an ongoing dogfood run/deployment.
