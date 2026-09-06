# Retry one failed independent task (#1501)

## Outcome

A person opens a failed independent Codex task, waits for its execution to stop,
uses the existing recovery action, and gets a new attempt under the same goal.
The successful result and the previous failure remain readable after refresh.
The new attempt starts in a new empty workspace; this is a new execution under
unchanged requirements. Existing outputs remain attached to the old attempt.

## Bounded change

Extend the existing recovery admission only for the already supported isolated
Codex route. Reuse the current allocation, frozen contract, recovery request,
scheduler, native reports and task details. No new API fields, migrations, task
ledger, provider configuration screen, file transfer, or live-process resume.
Existing recovery before worker preparation remains supported. Legacy workers
still need their own supported stop boundary before broader recovery can exist.

## Authority and stop evidence

- Task failure alone cannot authorize another execution. The original isolated
  Operation must have reached a failed terminal state, its start admission must
  be closed, and its persisted provider record must contain matching namespace
  quiescence evidence from the existing controller/runtime.
- Resolve and validate that evidence inside the current serialized recovery
  writer. Reuse the isolated journal's typed record validation; do not expose
  private provider fields or invent another stop receipt. Bind Track, task,
  operation, original request, endpoint and boundary identity exactly. Ambiguous
  operations, unsupported routes, missing/malformed/mismatched evidence, open or
  requested stops and live/unknown execution are not permission to retry.
- The same check protects admission, scheduling and preparation of the successor.
  Keep frozen requirements, author/release rules, stale-request and duplicate
  recovery guards. Old callbacks cannot write the new attempt. Worker-card or
  session deletion must not erase the authoritative retained Operation evidence.
- Reuse the existing UI capability and history. Explain that retry uses a new
  empty workspace when this route is eligible; no automatic retry or silent
  reuse of the previous workspace.

## Acceptance

1. Reproduce the current rejection after a real isolated fake-provider failure
   and runtime stop, then exercise the production recovery and scheduler path.
2. The recovered attempt uses the same logical key/unchanged goal, a different
   execution and workspace, and its accepted result is visible with the old
   failure in history. Duplicate/replayed requests produce only one successor.
3. Reject missing/wrong stop evidence, old live descendants, non-isolated workers,
   changed/withdrawn requirements and stale attempts. Test selected critical
   identity/admission guards with explicit production mutations.
4. Browser: failure and unavailable recovery → confirmed stopped/available →
   recovery action → running → success; refresh retains both attempt reports.
5. Perform a bounded parent-owned local Codex experiment, two independent final
   reviews and relevant local/CI checks. Merge this slice before another feature.
