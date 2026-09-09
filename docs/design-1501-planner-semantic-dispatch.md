# Planner semantic dispatch: one named independent task

Status: implementation scope, not a claim of shipped behavior. Baseline: b7bdad97c.
Related: #1501, #1492; result receipts shipped in #1593.

## Outcome

An authenticated internal Planner makes one `calm.task.dispatch` call with a readable task `name`, `goal`, `acceptance`, `executor: "codex"`, and `workspace: "empty"`. The server creates the existing Planner-authored task declaration, returns its system-generated task key and declaration locator with an honest current admission snapshot, and the existing scheduler and result receipts complete the loop. No report revision read, key generation, manual release shortcut, or run-directory search is required for this supported task.

The name is a Track-local business identity only for tasks created through Dispatch (not old card titles), not an opaque request token. Exact same name and normalized contract means the same logical task for the lifetime of the Track, including across response loss, new tool calls, and current Planner session replacement. A changed contract under that name conflicts; it never mutates or replaces work. Deliberately new work uses a new meaningful name; execution repair uses existing recovery rules. Names must be nonempty and at most 200 UTF-8 bytes after trim, with no remaining control characters. Only leading/trailing whitespace on the name is normalized; avoid case folding or invisible goal rewriting. The schema and tool description must state these semantics.

## Authority and persistence

Use the existing authenticated Planner-only MCP registry/DecisionSink path. Do not add a parallel native dynamic-tool dispatcher or general provider-turn ledger. Role, current session, Track scope, and recorder permission are checked inside the actual transaction, including receipt replay; an old session cannot retrieve a receipt to bypass authority.

A first Dispatch follows the existing Planner report declaration’s Draft-to-Planning auto-promotion after transactional authority and receipt lookup. Paused, Blocked and terminal tracks are not resumed. Receipt replay has no promotion or lifecycle side effects.

The report CRDT remains the declaration authority. Add a bounded dispatch purpose to the existing report writer; derive its revision inside that transaction, apply the same declaration guard, write the report, project tasks, and emit the existing events. Never write tasks rows as another source of truth. Add a new migration for the Track-local name-to-contract/task/block receipt, committed atomically with the declaration. Existing migrations remain byte-frozen. A receipt is immutable provenance/idempotency data, not a second mutable task status or acceptance flag. Track deletion and developer reset must respect its lifetime.

Replay checks the saved full typed contract, not a lossy goal-only hash. It does not rewrite a declaration that was edited, removed, released, recovered or completed later. Report changes are reflected as current state/explicit unavailability, not silent re-creation. Concurrent same-name requests converge on one declaration; different contracts conflict. Unrelated concurrent report edits survive.

## Supported work and response

Require explicit goal and semantic acceptance. The only supported executor/workspace enums are Codex and empty isolated workspace. The task uses existing isolated-codex-v1 selection, ready=true and Planner attribution. State explicitly that semantic acceptance is reviewed from the completion report; it is not a machine gate and does not qualify a file candidate. Unsupported executors, workspace modes, dependencies, arbitrary context/gate options and unknown fields are rejected before any write. No legacy fallback.

The result separates the durable creation receipt (name, generated key, report block locator) from a current authoritative snapshot (declaration/admission diagnostics, actual attempt when allocated, actual task state). Use existing projection/budget explanations. A declaration can await User release, capacity, lifecycle or another existing admission condition. `accepted` never means running; no synthetic attempt or Operation ID. Read a consistent current snapshot and accurately label the snapshot time. The existing eventized writer rejects empty batches; receipt replay follows the existing recovery convention of reading under authorization and rolling back with a private sentinel, returning the captured snapshot without any event write. Existing execution blocking-reason logic supplements declaration diagnostics for lifecycle waits. A later missing/deleted declaration remains a historical receipt with explicit unavailable/withdrawn state. Result delivery uses #1593 unchanged; worker completion is still distinct from execution settlement, verification and Planner acceptance.

## Acceptance checks

- Production MCP call: one supported dispatch creates one Planner declaration, expected projection and receipt; unrelated blocks remain intact. Scheduler/worker reporting reuses existing result path.
- Repeated same-name same-contract calls (also new call/session and simulated response loss) return original identity without new declarations/events/attempts. Changed contract conflicts. Concurrent requests pin the same invariant.
- Worker/Assistant, cross-Track/stale session and recorder denial cannot create or replay. Failed transaction cannot leave a receipt or declaration alone.
- Declare-and-wait, zero capacity and blocked lifecycle return truthful non-running reasons without bypassing User controls. Unsupported inputs leave zero writes.
- Replay after declaration edit/removal and task recovery preserves original receipt and does not recreate/release/repair work.
- Focused tests, selected critical mutation, two independent full-diff reviews, appropriate quick Rust gates and exact production prompt generation. One bounded private native Planner dialogue validates dispatch-to-result without touching 4140. Remote PR CI must be green before squash merge.

## Follow-ups

External CLI dispatch/wait/subscriptions, native dynamic-tool presentation, in-flight send, contract revision, file-input/candidate dispatch, multiple executor choices and rejected-candidate repair remain separate loops. This slice does not promise them.
