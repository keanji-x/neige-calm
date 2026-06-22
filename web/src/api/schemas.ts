// Zod schemas for WS `/api/events` payloads. The source of truth on the
// frontend for what the kernel can emit. Mirrors
// `crates/calm-server/src/event.rs`'s `Event` enum (serde-tagged
// `{ ev, data }`) and `crates/calm-server/src/model.rs`'s entity types.
//
// On parse failure, callers should log + skip dispatch — never throw — so a
// new event variant added server-side doesn't crash the UI. The runtime
// check exists to catch *unexpected* payload shapes (schema drift, partial
// rollouts, broken proxies) rather than to police every field.
//
// Inferred TS types are re-exported so consumers can opt in without touching
// the legacy `wire.ts` `WireEvent` union. The two are intentionally parallel
// today — a future migration can collapse them once all consumers move over.

import { z } from 'zod';

// ---------------- Entity schemas (mirror model.rs) ----------------

/**
 * Issue #175 — `model::CoveKind`. Marks whether a cove is part of the
 * user-visible workspace (`'user'`) or is the kernel-owned singleton that
 * hosts the default Today terminal's wave (`'system'`). The kernel already
 * filters `kind='system'` out of `GET /api/coves` by default, so this
 * frontend schema's main job is to type the field for the optional
 * belt-and-suspenders `.filter(c => c.kind === 'user')` in CalmApp /
 * router. Defaults to `'user'` so pre-#175 wire payloads (event-log
 * replay, legacy fixtures) round-trip without forcing a fixture rewrite.
 */
export const coveKindSchema = z.enum(['user', 'system']).default('user');
export type CoveKind = z.infer<typeof coveKindSchema>;

/** `model::Cove` — cove metadata row. */
export const coveSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: coveKindSchema,
  created_at: z.number(),
  updated_at: z.number(),
});

/**
 * Issue #145 — `model::WaveLifecycle`. Single source of truth for the
 * lifecycle state machine the Spec Agent drives. Wire values are
 * lowercase (`#[serde(rename_all = "lowercase")]` on the Rust enum).
 * `archived` is intentionally NOT a lifecycle state — archive is
 * orthogonal visibility on `wave.archived_at`.
 *
 * Defaults to `'draft'` for any pre-#145 wire payload (replay
 * fixtures, legacy event logs) — matches the DB DEFAULT in
 * migration 0012 and the `#[serde(default)]` on the Rust struct
 * field. Forces wave payloads emitted *before* the lifecycle column
 * existed to parse without a fixture rewrite.
 */
export const waveLifecycleSchema = z
  .enum([
    'draft',
    'planning',
    'dispatching',
    'working',
    'blocked',
    'reviewing',
    'done',
    'canceled',
    'failed',
  ])
  .default('draft');
export type WaveLifecycle = z.infer<typeof waveLifecycleSchema>;

/** `model::Wave` — wave metadata row. `archived_at` is `Option<i64>` server-side. */
export const waveSchema = z.object({
  id: z.string(),
  cove_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
  pinned_at: z.number().nullable().default(null),
  /**
   * Issue #145 — the wave's lifecycle state. Defaulted at the schema
   * layer to `'draft'` so a missing field on pre-#145 wire payloads
   * (event-log replay fixtures) parses cleanly. The kernel always
   * stamps a value on fresh writes.
   */
  lifecycle: waveLifecycleSchema,
  /**
   * Issue #250 PR 2 — wave's working directory (spec-daemon cwd).
   * Defaulted to `""` at the schema layer for symmetry with the
   * server-side `#[serde(default)]` on `Wave.cwd`: pre-#250 event-log
   * replay fixtures (no `cwd` key on `WaveUpdated`) parse cleanly.
   * Production rows always carry an absolute path.
   */
  cwd: z.string().default(''),
  /**
   * Issue #760 slice 4a — optional workflow descriptor backing this wave.
   * Defaulted to `null` for replay of event-log rows written before the
   * field existed; fresh rows serialize the field explicitly.
   */
  workflow_id: z.string().nullable().default(null),
  /**
   * Issue #250 PR 2 — unix-ms stamp the wave most recently entered a
   * terminal lifecycle state (Done / Canceled / Failed), or `null`
   * while non-terminal. Defaulted to `null` so pre-#250 wire payloads
   * (no key on the event) parse without churn.
   */
  terminal_at: z.number().nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});

export const runtimeKindSchema = z.enum(['terminal', 'codex', 'claude', 'shared-spec']);
export type WorkerSessionKind = z.infer<typeof runtimeKindSchema>;

export const agentProviderSchema = z.enum(['codex', 'claude']);
export type AgentProvider = z.infer<typeof agentProviderSchema>;

export const workerSessionStateSchema = z.enum([
  'starting',
  'running',
  'idle',
  'turn_pending',
  'failed',
  'exited',
  'superseded',
]);
export type WorkerSessionState = z.infer<typeof workerSessionStateSchema>;

export const cardRuntimeViewSchema = z.object({
  runtime_id: z.string(),
  kind: runtimeKindSchema,
  status: workerSessionStateSchema,
  provider: agentProviderSchema.optional(),
  terminal_id: z.string().optional(),
  thread_id: z.string().optional(),
  session_id: z.string().optional(),
  source: z.string().optional(),
  thread_status: z.string().optional(),
});
export type CardRuntimeView = z.infer<typeof cardRuntimeViewSchema>;

/** `model::Card` — card row. `payload` is opaque `serde_json::Value`. */
export const cardSchema = z.object({
  id: z.string(),
  wave_id: z.string(),
  kind: z.string(),
  sort: z.number(),
  // serde_json::Value on the wire: arbitrary JSON. Kernel never inspects.
  payload: z.unknown(),
  runtime: cardRuntimeViewSchema.optional(),
  // Issue #229 PR A — system-card guard bit. Kernel default = true
  // (matches the migration's `INTEGER NOT NULL DEFAULT 1`); the `#[serde(default
  // = "default_deletable")]` on the Rust struct means wire payloads
  // from pre-#229 servers / event-log replays may omit the field, and
  // zod surfaces that as `undefined`. The OpenAPI emitter renders the
  // field as optional too — matching `Card.deletable?: boolean` on the
  // generated TS. We `.default(true)` here so all downstream consumers
  // see a populated bool after parse, while still tolerating wire
  // omissions on the input side.
  deletable: z.boolean().default(true),
  created_at: z.number(),
  updated_at: z.number(),
});

/** `model::Overlay` — plugin overlay row. `payload` is opaque JSON. */
export const overlaySchema = z.object({
  id: z.string(),
  plugin_id: z.string(),
  // Documented as "wave" | "card" but kept open for forward-compat.
  entity_kind: z.string(),
  entity_id: z.string(),
  kind: z.string(),
  payload: z.unknown(),
  updated_at: z.number(),
});

// ---------------- Event schemas (mirror event.rs) ----------------

export const coveUpdatedSchema = z.object({
  ev: z.literal('cove.updated'),
  data: coveSchema,
});

export const coveDeletedSchema = z.object({
  ev: z.literal('cove.deleted'),
  data: z.object({ id: z.string() }),
});

export const waveUpdatedSchema = z.object({
  ev: z.literal('wave.updated'),
  data: waveSchema.extend({
    agent_message: z.string().optional(),
  }),
});

export const waveDeletedSchema = z.object({
  ev: z.literal('wave.deleted'),
  data: z.object({ id: z.string(), cove_id: z.string() }),
});

/**
 * Issue #145 — `Event::WaveLifecycleChanged`. Emitted exactly once per
 * validated `from → to` transition. Reducers downstream can subscribe to
 * `kind = wave.lifecycle_changed` directly without inspecting every
 * `wave.updated` for a possibly-unchanged `lifecycle` field. Wave-scoped
 * (routes to `wave:<id>` and `cove:<cove>` topics).
 */
export const waveLifecycleChangedSchema = z.object({
  ev: z.literal('wave.lifecycle_changed'),
  data: z.object({
    id: z.string(),
    cove_id: z.string(),
    from: waveLifecycleSchema,
    to: waveLifecycleSchema,
    agent_message: z.string().optional(),
  }),
});

export const cardAddedSchema = z.object({
  ev: z.literal('card.added'),
  data: cardSchema,
});

export const cardUpdatedSchema = z.object({
  ev: z.literal('card.updated'),
  data: cardSchema,
});

export const cardDeletedSchema = z.object({
  ev: z.literal('card.deleted'),
  data: z.object({ id: z.string(), wave_id: z.string() }),
});

export const runtimeStartedSchema = z.object({
  ev: z.literal('runtime.started'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    kind: runtimeKindSchema,
    agent_provider: agentProviderSchema.nullable(),
    status: workerSessionStateSchema,
  }),
});

export const runtimeStatusChangedSchema = z.object({
  ev: z.literal('runtime.status_changed'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    old_status: workerSessionStateSchema,
    new_status: workerSessionStateSchema,
  }),
});

export const runtimeSupersededSchema = z.object({
  ev: z.literal('runtime.superseded'),
  data: z.object({
    old_runtime_id: z.string(),
    new_runtime_id: z.string(),
    card_id: z.string(),
  }),
});

export const harnessPhaseTagSchema = z.enum([
  'pending_thread_start',
  'idle',
  'issuing_turn',
  'issuing_interrupt',
  'turn_running',
  'turn_completed',
  'resumed',
  'wedged',
]);
export type HarnessPhaseTag = z.infer<typeof harnessPhaseTagSchema>;

export const harnessItemAddedSchema = z.object({
  ev: z.literal('harness.item.added'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    wave_id: z.string(),
    item_db_id: z.number(),
    item_uuid: z.string().nullable(),
    item_type: z.string().nullable(),
    turn_id: z.string().nullable(),
    method: z.string(),
  }),
});

export const harnessPhaseChangedSchema = z.object({
  ev: z.literal('harness.phase.changed'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    wave_id: z.string(),
    old_phase: harnessPhaseTagSchema,
    new_phase: harnessPhaseTagSchema,
  }),
});

export const harnessTranscriptClearedSchema = z.object({
  ev: z.literal('harness.transcript.cleared'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    wave_id: z.string(),
  }),
});

export const harnessUserMessageEnqueuedSchema = z.object({
  ev: z.literal('harness.user_message.enqueued'),
  data: z.object({
    runtime_id: z.string(),
    card_id: z.string(),
    wave_id: z.string(),
    char_count: z.number(),
  }),
});

/**
 * Issue #247 PR2 — `Event::WaveReportEdited`. Structured edit-log
 * companion to `card.updated` emitted from every wave-report write.
 * `card.updated` stays the generic "row changed, re-fetch" signal
 * existing frontend subscribers consume; `wave.report_edited` is the
 * *additional* timeline entry the new edit-history UI (PR4) and the
 * spec agent's user-edit notifier (PR5) read.
 *
 * `author` discriminates who produced the edit. PR2 only emits
 * `'spec'`; PR3 introduces `'user'` for REST-driven edits; `'kernel'`
 * is reserved for future server-internal rewrites.
 *
 * `edit_id` is a fresh UUID v4 per call so the UI can collapse
 * adjacent retries or correlate timeline entries with a future
 * REST-side request id without parsing the `_id` envelope field.
 *
 * Card-scoped on the persisted events row (`scope_wave = wave_id`,
 * `scope_card = card_id`).
 */
export const waveReportEditedSchema = z.object({
  ev: z.literal('wave.report_edited'),
  data: z.object({
    wave_id: z.string(),
    card_id: z.string(),
    author: z.enum(['spec', 'user', 'kernel']),
    edit_id: z.string(),
    summary_before: z.string(),
    summary_after: z.string(),
    body_before: z.string(),
    body_after: z.string(),
    agent_message: z.string().optional(),
  }),
});

export const overlaySetSchema = z.object({
  ev: z.literal('overlay.set'),
  data: overlaySchema,
});

export const overlayDeletedSchema = z.object({
  ev: z.literal('overlay.deleted'),
  data: z.object({
    plugin_id: z.string(),
    entity_kind: z.string(),
    entity_id: z.string(),
    kind: z.string(),
  }),
});

/**
 * `Event::TerminalDeleted` — emitted by the orphan-terminal sweeper
 * (`crates/calm-server/src/terminal_sweeper.rs`) when a terminal row is
 * reaped because no card payload references it anymore. Actor is
 * `"kernel"` on the events-table row. Topic: `terminal:<id>`. The UI
 * doesn't currently subscribe to per-terminal topics, but the schema is
 * carried here so the runtime validator accepts the frame on the
 * firehose (`*`) subscription without dispatch-mismatch warnings.
 */
export const terminalDeletedSchema = z.object({
  ev: z.literal('terminal.deleted'),
  data: z.object({
    id: z.string(),
    card_id: z.string(),
  }),
});

/**
 * `Event::PluginState` — emitted by the plugin host on lifecycle transitions.
 * `state` is a free-form string (e.g. `"Spawning"`, `"Running"`, `"Crashed"`)
 * matching the Rust `PluginState` enum's `Display`. `last_error` is `None`
 * for healthy transitions and `Some(msg)` on crash / init-rejected paths
 * (skipped from serialization when `None`, so the field is optional here).
 */
export const pluginStateSchema = z.object({
  ev: z.literal('plugin.state'),
  data: z.object({
    id: z.string(),
    state: z.string(),
    last_error: z.string().optional(),
  }),
});

/**
 * `Event::PluginToolRegistered` — boot-time announcement for one
 * manifest-declared plugin MCP tool that is currently running and exposed
 * through the kernel MCP server as `plugin.<plugin_id>.<tool_name>`.
 */
export const pluginToolRegisteredSchema = z.object({
  ev: z.literal('plugin.tool.registered'),
  data: z.object({
    plugin_id: z.string(),
    tool_name: z.string(),
  }),
});

/**
 * `Event::WorkflowRegistered` — boot-time announcement for one plugin
 * workflow descriptor. No frontend query consumes the workflow catalog yet;
 * the event is still part of the exhaustive wire contract.
 */
export const workflowRegisteredSchema = z.object({
  ev: z.literal('workflow.registered'),
  data: z.object({
    pluginId: z.string(),
    workflowId: z.string(),
  }),
});

/**
 * `Event::CodexHook` — passthrough of one codex-CLI hook firing
 * (PreToolUse / PostToolUse / Stop / ...). `kind` carries a snake-case
 * discriminator (`hook.codex.<event>`) so callers can pattern-match
 * without typing every codex payload field. `payload` is the raw codex
 * JSON, kept opaque.
 */
export const codexHookSchema = z.object({
  ev: z.literal('codex.hook'),
  data: z.object({
    card_id: z.string(),
    kind: z.string(),
    hook_idempotency_key: z.string(),
    payload: z.unknown(),
  }),
});

/**
 * `Event::ClaudeHook` — passthrough of one Claude hook firing.
 * Mirrors `codexHookSchema`; `payload` stays opaque to the web layer.
 */
export const claudeHookSchema = z.object({
  ev: z.literal('claude.hook'),
  data: z.object({
    card_id: z.string(),
    kind: z.string(),
    hook_idempotency_key: z.string(),
    payload: z.unknown(),
  }),
});

// ---------------- PR4 of #136: dispatcher + task-lifecycle variants ----
//
// Schema-only PR — no kernel emitters today. PR5 (Dispatcher) wires them.
// The four schemas below pin the wire shape the kernel will start emitting
// once PR5 lands, so the runtime validator at the WS boundary doesn't drop
// frames on the floor.
//
// `ArtifactRef` is a transparent newtype on the server (#129 placeholder);
// ts-rs emits `export type ArtifactRef = string;` so on the wire each
// element of `task.completed.artifacts[]` is a bare string.

/**
 * `Event::CodexWorkerRequested` — spec/worker card asks the kernel
 * dispatcher to spawn a codex worker card. PR5's `Dispatcher` consumes
 * via `EventBus::subscribe(kinds=["*.requested"])` and correlates the
 * eventual `task.completed` / `task.failed` back to the requester via
 * `idempotency_key`.
 *
 * `context` is opaque `serde_json::Value` (working-dir hints, prior turn
 * history, model preference) — kernel never inspects, dispatcher
 * forwards verbatim into the spawned worker's card payload.
 */
export const codexWorkerRequestedSchema = z.object({
  ev: z.literal('codex.worker_requested'),
  data: z.object({
    idempotency_key: z.string(),
    goal: z.string(),
    context: z.unknown(),
    acceptance_criteria: z.string().optional(),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::TerminalWorkerRequested` — spec card asks the dispatcher to spawn
 * a terminal worker card. `cwd` is `None` when the spec card defers to
 * the wave/cove default working directory.
 */
export const terminalWorkerRequestedSchema = z.object({
  ev: z.literal('terminal.worker_requested'),
  data: z.object({
    idempotency_key: z.string(),
    cmd: z.string(),
    cwd: z.string().optional(),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::TaskCompleted` — worker card reports task completion.
 * `idempotency_key` echoes the matching `*.worker_requested` key so the
 * spec can correlate without parsing the worker card's identity.
 *
 * `artifacts` is `Vec<ArtifactRef>` server-side; `ArtifactRef` is a
 * transparent newtype around `String`, so each element is a bare string
 * on the wire. #129 will expand the type with hash / content-type /
 * storage-uri — at that point this schema will tighten alongside.
 */
export const taskCompletedSchema = z.object({
  ev: z.literal('task.completed'),
  data: z.object({
    idempotency_key: z.string(),
    result: z.unknown(),
    artifacts: z.array(z.string()),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::TaskFailed` — worker card reports task failure. `reason` is a
 * free-form failure string; the kernel never parses it but persists it
 * on the events table so audit-log replay can surface the rationale the
 * worker gave its spec.
 */
export const taskFailedSchema = z.object({
  ev: z.literal('task.failed'),
  data: z.object({
    idempotency_key: z.string(),
    reason: z.string(),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::PlanUpdated` — issue #644: the spec revised the wave's task
 * plan via `calm.plan.upsert` / `calm.plan.cancel`. Wave-scoped audit
 * record; `changed_keys` lists the task keys whose rows were
 * created/updated/canceled by the call (`unchanged` upserts are not
 * listed). The PR-B scheduler subscribes to this kind as its primary
 * trigger; no web query consumes the tasks table yet.
 */
export const planUpdatedSchema = z.object({
  ev: z.literal('plan.updated'),
  data: z.object({
    wave_id: z.string(),
    changed_keys: z.array(z.string()),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::TaskDispatched` — issue #644 PR-B: the kernel scheduler
 * claimed a plan task (`pending → dispatched`), appended inside the
 * claim tx. `idempotency_key` is the task id (`"{wave_id}:{key}"`);
 * `kind` is the worker kind (`"codex"` / `"terminal"` — a plain string
 * so a future worker kind is not a wire break). Kernel-only (actor
 * `KernelDispatcher`); the runs views treat it as the requested-record
 * fallback for scheduler-dispatched tasks.
 */
export const taskDispatchedSchema = z.object({
  ev: z.literal('task.dispatched'),
  data: z.object({
    idempotency_key: z.string(),
    kind: z.string(),
    agent_message: z.string().optional(),
  }),
});

/**
 * `Event::WorkspaceLeased` — issue #760 slice 1: the kernel created an
 * isolated workspace directory for a Codex worker card.
 */
export const workspaceLeasedSchema = z.object({
  ev: z.literal('workspace.leased'),
  data: z.object({
    wave_id: z.string(),
    card_id: z.string(),
    lease_id: z.string(),
    path: z.string(),
  }),
});

/**
 * `Event::WorkspaceReleased` — issue #760 slice 1: the kernel released the
 * durable workspace lease after completion, compensation, or boot reclaim.
 */
export const workspaceReleasedSchema = z.object({
  ev: z.literal('workspace.released'),
  data: z.object({
    wave_id: z.string(),
    card_id: z.string(),
    lease_id: z.string(),
  }),
});

export const forgeMergeSubjectSchema = z.object({
  phase: z.string(),
  slice_id: z.string(),
  pr_number: z.number(),
});

export const reviewSubjectSchema = z.object({
  phase: z.string(),
  slice_id: z.string(),
  pr_number: z.number().nullable(),
});

export const channelVerdictSchema = z.object({
  role: z.string(),
  verdict: z.string(),
});

export const ratifyDecisionSchema = z.enum(['grant', 'deny']);

/**
 * `Event::ForgePrMerged` — issue #760 slice 6: the forge action adapter
 * observed a PR merge and atomically completed the parked operation.
 */
export const forgePrMergedSchema = z.object({
  ev: z.literal('forge.pr.merged'),
  data: z.object({
    wave_id: z.string(),
    subject: forgeMergeSubjectSchema,
    head_sha: z.string(),
    merge_sha: z.string(),
  }),
});

/**
 * `Event::ReviewRound` — issue #760 slice ⑤-b-i: the spec recorded one
 * dual-review convergence round for a logical review subject.
 */
export const reviewRoundSchema = z.object({
  ev: z.literal('review.round'),
  data: z.object({
    wave_id: z.string(),
    subject: reviewSubjectSchema,
    head_sha: z.string().nullable(),
    n: z.number(),
    cap: z.number(),
    converged: z.boolean(),
    channels: z.array(channelVerdictSchema),
    root_cause: z.string().nullable(),
    idempotency_key: z.string(),
  }),
});

export const ratifyRequestedSchema = z.object({
  ev: z.literal('ratify.requested'),
  data: z.object({
    wave_id: z.string(),
    reason: z.string(),
  }),
});

export const ratifyResolvedSchema = z.object({
  ev: z.literal('ratify.resolved'),
  data: z.object({
    wave_id: z.string(),
    decision: ratifyDecisionSchema,
  }),
});

export const forgeScanCompletedSchema = z.object({
  ev: z.literal('forge.scan.completed'),
  data: z.object({
    wave_id: z.string(),
    overlapping_prs: z.array(z.number()),
  }),
});

export const forgePrOpenedSchema = z.object({
  ev: z.literal('forge.pr.opened'),
  data: z.object({
    wave_id: z.string(),
    pr_number: z.number(),
    head_sha: z.string(),
  }),
});

export const forgePrDiffReadSchema = z.object({
  ev: z.literal('forge.pr.diff.read'),
  data: z.object({
    wave_id: z.string(),
    pr_number: z.number(),
    base_sha: z.string(),
    head_sha: z.string(),
    artifact_path: z.string(),
  }),
});

export const forgePrChecksSchema = z.object({
  ev: z.literal('forge.pr.checks'),
  data: z.object({
    wave_id: z.string(),
    pr_number: z.number(),
    conclusion: z.string(),
  }),
});

export const forgeIssueReadSchema = z.object({
  ev: z.literal('forge.issue.read'),
  data: z.object({
    wave_id: z.string(),
    issue_number: z.number(),
    artifact_path: z.string(),
  }),
});

export const forgeIssueClosedSchema = z.object({
  ev: z.literal('forge.issue.closed'),
  data: z.object({
    wave_id: z.string(),
    issue_number: z.number(),
  }),
});

export const worktreeProvisionedSchema = z.object({
  ev: z.literal('worktree.provisioned'),
  data: z.object({
    wave_id: z.string(),
    card_id: z.string(),
    path: z.string(),
  }),
});

export const worktreeRemovedSchema = z.object({
  ev: z.literal('worktree.removed'),
  data: z.object({
    wave_id: z.string(),
    card_id: z.string(),
    path: z.string(),
  }),
});

/**
 * `Event::TaskGateResult` — issue #644 PR-C: the kernel gate runner
 * finished one `task-verify` attempt; appended in the same tx as the
 * `verifying → done|failed` tasks-row flip. `task_id` and
 * `idempotency_key` both carry the task id (`"{wave_id}:{key}"`).
 * Kernel-only (actor `KernelDispatcher`). `failing_step` / `exit_code`
 * are absent on the wire for verdicts that don't carry them (skip-if-
 * none serde on the Rust side).
 */
export const taskGateResultSchema = z.object({
  ev: z.literal('task.gate_result'),
  data: z.object({
    task_id: z.string(),
    idempotency_key: z.string(),
    passed: z.boolean(),
    failing_step: z.string().optional(),
    exit_code: z.number().optional(),
    log_tail: z.string(),
    log_path: z.string(),
    attempt: z.number(),
    agent_message: z.string().optional(),
  }),
});

// ---------------- EventScope (mirror event.rs) ----------------

/**
 * `EventScope` — the event's "home scope" in the cove → wave → card
 * hierarchy. PR2 of #136 adds this to every persisted event so future
 * MCP subscribers / dispatcher routes can filter without re-parsing
 * the payload. Tagged `{kind, id}` shape via `#[serde(tag, content)]`
 * on the Rust side.
 *
 * `System` is the catch-all for events that genuinely don't belong to
 * a single cove/wave/card (`plugin.state`, cove-create, the pre-PR2
 * NULL-fallback). Pre-PR2 history rows replay as `System`.
 */
export const eventScopeSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('System') }),
  z.object({ kind: z.literal('Cove'), id: z.object({ cove: z.string() }) }),
  z.object({
    kind: z.literal('Wave'),
    id: z.object({ wave: z.string(), cove: z.string() }),
  }),
  z.object({
    kind: z.literal('Card'),
    id: z.object({ card: z.string(), wave: z.string(), cove: z.string() }),
  }),
]);

export type EventScope = z.infer<typeof eventScopeSchema>;

// ---------------- Discriminated union ----------------

/**
 * The complete set of events the kernel can push on `/api/events`. Keep this
 * 1:1 with `event::Event` in calm-server; the WS handler runtime-validates
 * each frame through this schema and skips dispatch on mismatch.
 */
export const wireEventSchema = z.discriminatedUnion('ev', [
  coveUpdatedSchema,
  coveDeletedSchema,
  waveUpdatedSchema,
  waveDeletedSchema,
  waveLifecycleChangedSchema,
  cardAddedSchema,
  cardUpdatedSchema,
  cardDeletedSchema,
  runtimeStartedSchema,
  runtimeStatusChangedSchema,
  runtimeSupersededSchema,
  harnessItemAddedSchema,
  harnessPhaseChangedSchema,
  harnessTranscriptClearedSchema,
  harnessUserMessageEnqueuedSchema,
  waveReportEditedSchema,
  overlaySetSchema,
  overlayDeletedSchema,
  terminalDeletedSchema,
  pluginStateSchema,
  pluginToolRegisteredSchema,
  workflowRegisteredSchema,
  codexHookSchema,
  claudeHookSchema,
  codexWorkerRequestedSchema,
  terminalWorkerRequestedSchema,
  taskCompletedSchema,
  taskFailedSchema,
  planUpdatedSchema,
  taskDispatchedSchema,
  workspaceLeasedSchema,
  workspaceReleasedSchema,
  forgePrMergedSchema,
  reviewRoundSchema,
  ratifyRequestedSchema,
  ratifyResolvedSchema,
  forgeScanCompletedSchema,
  forgePrOpenedSchema,
  forgePrDiffReadSchema,
  forgePrChecksSchema,
  forgeIssueReadSchema,
  forgeIssueClosedSchema,
  worktreeProvisionedSchema,
  worktreeRemovedSchema,
  taskGateResultSchema,
]);

// ---------------- Inferred types ----------------
//
// Available for consumers that want a stronger type than `WireEvent` from
// `wire.ts`. Not migrated yet — the two coexist by design until a sweep.

export type Cove = z.infer<typeof coveSchema>;
export type Wave = z.infer<typeof waveSchema>;
export type Card = z.infer<typeof cardSchema>;
export type Overlay = z.infer<typeof overlaySchema>;

export type CoveUpdatedEvent = z.infer<typeof coveUpdatedSchema>;
export type CoveDeletedEvent = z.infer<typeof coveDeletedSchema>;
export type WaveUpdatedEvent = z.infer<typeof waveUpdatedSchema>;
export type WaveDeletedEvent = z.infer<typeof waveDeletedSchema>;
export type WaveLifecycleChangedEvent = z.infer<typeof waveLifecycleChangedSchema>;
export type CardAddedEvent = z.infer<typeof cardAddedSchema>;
export type CardUpdatedEvent = z.infer<typeof cardUpdatedSchema>;
export type CardDeletedEvent = z.infer<typeof cardDeletedSchema>;
export type RuntimeStartedEvent = z.infer<typeof runtimeStartedSchema>;
export type RuntimeStatusChangedEvent = z.infer<typeof runtimeStatusChangedSchema>;
export type RuntimeSupersededEvent = z.infer<typeof runtimeSupersededSchema>;
export type HarnessItemAddedEvent = z.infer<typeof harnessItemAddedSchema>;
export type HarnessPhaseChangedEvent = z.infer<typeof harnessPhaseChangedSchema>;
export type HarnessTranscriptClearedEvent = z.infer<
  typeof harnessTranscriptClearedSchema
>;
export type HarnessUserMessageEnqueuedEvent = z.infer<
  typeof harnessUserMessageEnqueuedSchema
>;
export type WaveReportEditedEvent = z.infer<typeof waveReportEditedSchema>;
export type OverlaySetEvent = z.infer<typeof overlaySetSchema>;
export type OverlayDeletedEvent = z.infer<typeof overlayDeletedSchema>;
export type TerminalDeletedEvent = z.infer<typeof terminalDeletedSchema>;
export type PluginStateEvent = z.infer<typeof pluginStateSchema>;
export type PluginToolRegisteredEvent = z.infer<typeof pluginToolRegisteredSchema>;
export type WorkflowRegisteredEvent = z.infer<typeof workflowRegisteredSchema>;
export type CodexHookEvent = z.infer<typeof codexHookSchema>;
export type ClaudeHookEvent = z.infer<typeof claudeHookSchema>;
export type CodexWorkerRequestedEvent = z.infer<typeof codexWorkerRequestedSchema>;
export type TerminalWorkerRequestedEvent = z.infer<typeof terminalWorkerRequestedSchema>;
export type TaskCompletedEvent = z.infer<typeof taskCompletedSchema>;
export type TaskFailedEvent = z.infer<typeof taskFailedSchema>;
export type PlanUpdatedEvent = z.infer<typeof planUpdatedSchema>;
export type TaskDispatchedEvent = z.infer<typeof taskDispatchedSchema>;
export type WorkspaceLeasedEvent = z.infer<typeof workspaceLeasedSchema>;
export type WorkspaceReleasedEvent = z.infer<typeof workspaceReleasedSchema>;
export type ForgePrMergedEvent = z.infer<typeof forgePrMergedSchema>;
export type ReviewRoundEvent = z.infer<typeof reviewRoundSchema>;
export type RatifyRequestedEvent = z.infer<typeof ratifyRequestedSchema>;
export type RatifyResolvedEvent = z.infer<typeof ratifyResolvedSchema>;
export type ForgeScanCompletedEvent = z.infer<typeof forgeScanCompletedSchema>;
export type ForgePrOpenedEvent = z.infer<typeof forgePrOpenedSchema>;
export type ForgePrDiffReadEvent = z.infer<typeof forgePrDiffReadSchema>;
export type ForgePrChecksEvent = z.infer<typeof forgePrChecksSchema>;
export type ForgeIssueReadEvent = z.infer<typeof forgeIssueReadSchema>;
export type ForgeIssueClosedEvent = z.infer<typeof forgeIssueClosedSchema>;
export type WorktreeProvisionedEvent = z.infer<typeof worktreeProvisionedSchema>;
export type WorktreeRemovedEvent = z.infer<typeof worktreeRemovedSchema>;
export type TaskGateResultEvent = z.infer<typeof taskGateResultSchema>;

export type WireEvent = z.infer<typeof wireEventSchema>;
