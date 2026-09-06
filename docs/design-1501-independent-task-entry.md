# One user-facing independent-task loop (#1501)

## Outcome

Within an existing Track, a person enters a goal, starts one Codex task in a new
empty workspace, and reads its completion report or failure in the task details.
Use the existing scheduler, isolated execution, history and safe recovery. No
repository delivery, artifact publication, backend configuration UI, or new task
state store is part of this slice.

## Boundaries

- Add a narrow authenticated User-only task-start API. It creates the fixed
  isolated Codex task declaration through the existing report writer and promotes
  a Draft Track in that same transaction. The UI states that this starts the Track
  (and may release its other ready work). An ordinary report edit remains an edit.
  An unavailable configured backend or non-runnable/terminal Track is rejected
  before a task is authored; the action does not silently resume blocked work.
- The request carries a client-minted task key, goal and expected report revision.
  Repeated requests retain these exact values. Revision conflicts cannot silently
  obtain a new revision and create another task. Existing keys cannot be overwritten
  or duplicated by this task-start purpose. No generic extra writer callback or
  second scheduler is introduced.
- After an uncertain response, the frontend checks the existing declaration/history
  under that key and retries only the unchanged request. It must not create a new
  intent automatically. Double activation and navigation must not duplicate a task.
- Add an authenticated read of the accepted native report for an exact attempt,
  scoped to its Track and task key. Read the existing durable task/event evidence;
  do not infer results from process exit, filesystem paths or a mutable card payload.
  No new persistent result copy is introduced. No report is distinct from a report
  whose result is JSON null. Failure state already present in history remains visible.
- UI text describes an independent empty workspace and the actual outcome. Internal
  selection tags, CLI options and private provider paths are not configuration forms
  or artifact download links. Render model-provided content safely as text/JSON.

## Proposed API contract (freeze before parallel implementation)

`POST /api/tracks/{id}/isolated-tasks`

Request: `{ "key": "independent-<client nonce>", "goal": "...", "ifDocRev": 12 }`.
Success: `{ "taskKey": "...", "blockId": "...", "docRev": 13 }`.
The server fixes kind/empty-workspace selection, User release and report-driven
acceptance reason. It validates the existing Track lifecycle, revision and key
uniqueness at the authoritative write boundary. A conflict is not a fresh intent.

`GET /api/tracks/{id}/tasks/{key}/attempts/{attempt_id}/report`

Response: `{ "attemptId": "...", "report": null }` when no report exists, or
`report: { "kind": "completed", "result": <JSON>, "artifacts": ["..."] }`, or
`report: { "kind": "failed", "reason": "..." }`. Only the exact scoped attempt's
accepted report is returned. Artifact strings are reported evidence, not published files.

## Acceptance

1. Browser goal submission authors one task and starts a Draft Track atomically.
2. Actual configured isolated execution reaches running and reports completion;
   task details show the report. Refresh retains it.
3. Double submit, unknown response, revision/key conflict and unavailable backend
   preserve intent and cannot duplicate/overwrite a task.
4. Failed/no-report cases remain understandable; wrong Track/key/attempt reads fail
   closed and model text cannot execute in the page.
5. Use two independent reviews and relevant local/CI gates before squash. Record
   follow-ups in #1501 and stop after this loop is delivered.

## Contract ownership decision (CR-1501-API)

The generated OpenAPI contract is owned by `core` and frozen. This slice requests
only the two additive endpoints and their request/response schemas specified
above. The parent orchestrator approves this narrow change for #1501; both
implementation agents use that contract. Existing wire fields and ownership
rules remain unchanged. The actual Rust generator produced the frontend spec;
its commit and squash body must retain this exact approval trailer:

OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — regenerate approved independent task start and accepted report contract (#1501)
