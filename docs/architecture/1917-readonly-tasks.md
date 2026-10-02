# #1917 Read-only tasks share the Track checkout

Replaces the PR #1921 design. Owner decisions (2026-10-02): enforcement is by declaration only;
#1921 is closed and its branch kept for reference; work starts from current main.

## Pain point

Since #1830 S2 every codex/claude task of a track runs one at a time in the Track checkout
(`track_idle`, `docs/architecture/1830-s2-worker-in-track-worktree.md` KNOWN GAPS: "Managed tracks
lose parallel workers"). In 4140 the second reviewer of every dual-review pair now waits for the
first (tracks 676b3df5, 28dffbaf, e0646de4: wait = the sibling's run, 2–6 min). Recent reviewers
are mostly `claude` tasks.

## Why #1921 grew to 24k lines, and what this design does not do

#1921 required proof that no writer can be executing before a reader may share the checkout. On
main that is false by design: a writer's lease is released at its report (`decision_sink.rs`
D7), and the Planner, terminals and forge actions write without a lease. #1830 accepted these as
KNOWN GAPS. Making the proof true needed an execution manager, a TUI protocol proxy, nonce
recovery, background-terminal and descendant-thread stop evidence, and legacy-scope discovery.
None of that is caused by readers, so none of it is here.

The only new hazard readers add is a reader and a writer running at the same time. Scheduler
admission prevents it. A reader that reported but whose process is still running reads late and
harms nobody: its report is already in.

## Design

1. **Declaration.** A task block gains `access: "read_only" | "read_write"` (absent =
   `read_write`). It is stored as a typed `tasks.access` column (new migration, `NOT NULL DEFAULT
   'read_write'`, CHECK). A `read_only` task must be kind `codex` or `claude`, run in the track
   (not the child-track route), and have no `gate`. It counts as gate-exempt under
   `require_task_gates`, the same as `no_gate_reason`.
2. **Leases.** A reader still takes a `workspace_leases` row, so forge cwd, card delete, reaper,
   boot reclaim and `neige state` keep working unchanged. The row gets a new
   `access_mode` column (same migration, default `read_write`). A reader lease has no base and no
   `delivery_policy`, so its release writes no delivery row and the task ends `done` directly
   (ungated success already flips to `Done`). The active-path unique index becomes unique among
   `read_write` rows only.
3. **Admission (one function).** `track_idle` becomes an occupancy read: `Free` (nothing in
   flight), `Readers` (only read-only tasks/leases in flight, no unsettled delivery) or `Busy`. A
   writer needs `Free`; a reader needs `Free` or `Readers`. The claim transaction rechecks with
   the claimed task's access. Pending diagnostics (`TrackBusy`) use the same function.
4. **No writer starvation.** In scheduler order, once a deps-ready writer is waiting, later
   readers are not admitted. Readers ahead of it still run. Scheduler and pending projection call
   the same ordering function.
5. **Prompt.** `planner.md`/gates guide: a review or audit task declares `access: read_only` and
   needs no gate; it may run beside other read-only tasks. The worker prompt for a reader says it
   must not modify the checkout. Byte ratchets stay at or under their baselines.

Facts the implementation settled (Phase 0):

- `access` is stored like `spawn`: it updates a `pending` row, stays out of the released task
  root-hash partition and out of in-flight drift.
- The claim transaction re-runs the whole admission rule (occupancy apart from itself plus the
  starvation rule) over the current plan, so a reader cannot slip past a writer that started
  waiting during the same pass.
- `calm.plan.list` shows a reader's candidate as `binding: none, reason: read_only` (a base-less
  lease would otherwise read as `unbound: legacy_lease`), and each entry carries `access`.

## Not enforced (KNOWN GAPS)

- A reader is trusted not to write (no sandbox change; codex keeps `workspace-write`, so reviewers
  can still run checks). A reader that dirties the tree makes the next task (reader or writer)
  fail `ensure_clean_tree`, and the Planner cleans it, as for terminal tasks today.
- The Planner, terminals and forge actions still write without a lease, and a writer's lease is
  still released at its report. Unchanged from #1830.
- Readers already running when a writer becomes ready keep it waiting until they report; only
  later readers are held back.
- A reader lease whose owner op is `stuck` stops counting, like a writer's; the next attempt's
  prepare (reader or writer) supersedes it.

## Compatibility (4140)

All 110 `workspace_leases` rows are `released`; every existing task and lease row takes the
`read_write` default, so behaviour is unchanged until a task declares `read_only`.

## Acceptance

- Two read-only tasks of one track are dispatched concurrently; a writer waits for both; readers
  wait for a running writer and for an unsettled delivery.
- A deps-ready writer ahead in order holds back later readers.
- A reader's report releases its lease with no delivery row and the task is `done`.
- Invalid declarations (`read_only` with a gate, terminal kind, child-track route) are rejected.
- Pending diagnostics agree with admission (same function).
- Must-red: mutating the occupancy predicate (reader treated as writer, writer treated as reader)
  and the starvation rule reds the named admission tests.
