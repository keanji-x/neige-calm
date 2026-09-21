// Zod schemas for WS `/api/events` payloads, mirroring the kernel's `Event` enum and entity types.
// On parse failure, callers log and skip dispatch — never throw.
import { z } from 'zod';

import type { ApiDecodeFailure } from './types.js';

/** `model::AreaKind`. Defaults to `'user'` so legacy wire payloads parse. */
export const areaKindSchema = z.enum(['user', 'system']).default('user');
export type AreaKind = z.infer<typeof areaKindSchema>;

/** `model::Area` — area metadata row. */
export const areaSchema = z.object({
  id: z.string(),
  name: z.string(),
  color: z.string(),
  sort: z.number(),
  kind: areaKindSchema,
  default_template_id: z.string().nullable().default(null),
  default_cwd: z.string().nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});

/**
 * `model::TrackLifecycle`. `archived` is intentionally NOT a lifecycle state; defaults to `'draft'`
 * for legacy payloads.
 */
export const trackLifecycleSchema = z
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
export type TrackLifecycle = z.infer<typeof trackLifecycleSchema>;

/**
 * One-way read compatibility for the pre-rename track keys (`workflow_id` / `workflow_input`).
 * Deliberately a preprocess step and NOT an optional field on the schema, and deliberately
 * not shared with the other zod readers.
 */
function normalizeLegacyTemplateKeys(raw: unknown): unknown {
  if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return raw;
  const row = raw as Record<string, unknown>;
  if (!('workflow_id' in row) && !('workflow_input' in row)) return raw;
  const { workflow_id: legacyId, workflow_input: legacyInput, ...rest } = row;
  return {
    ...rest,
    ...(rest.template_id === undefined && legacyId !== undefined
      ? { template_id: legacyId }
      : {}),
    ...(rest.template_input === undefined && legacyInput !== undefined
      ? { template_input: legacyInput }
      : {}),
  };
}

/** A Claude Code permission scope: `edit` globs relative to the terminal cwd, `bash` prefixes, `deny` prefixes. */
export const claudePermissionsScopeSchema = z.object({
  edit: z.array(z.string()).optional(),
  bash: z.array(z.string()).optional(),
  deny: z.array(z.string()).optional(),
});
export type ClaudePermissionsScope = z.infer<typeof claudePermissionsScopeSchema>;

const trackObjectSchema = z.object({
  id: z.string(),
  area_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
  pinned_at: z.number().nullable().default(null),
  lifecycle: trackLifecycleSchema,
  /** Defaulted to `""` for legacy replay payloads; production rows always carry an absolute path. */
  cwd: z.string().default(''),
  template_id: z.string().nullable().default(null),
  plugin_scope: z.string().nullable().default(null),
  purpose: z.string().nullable().default(null),
  /** Opaque bound-template input JSON; the frontend never interprets it. */
  template_input: z.unknown().default(null),
  /** Unix-ms stamp the track most recently entered a terminal lifecycle state, or `null` while non-terminal. */
  terminal_at: z.number().nullable().default(null),
  /** Server-owned provenance; both are `null` together for tracks that came from anywhere else. */
  recipe_id: z.string().nullable().default(null),
  recipe_revision: z.number().nullable().default(null),
  /** The typed workspace; `cwd` above is a projection of `workspace.path`. */
  workspace: z
    .object({
      // No per-field defaults, deliberately: a missing `workspace` key must parse, but a
      // present-and-incomplete one is a server regression. Matches serde, not OpenAPI.
      kind: z.enum(['managed', 'attached']),
      path: z.string(),
      frozen_at: z.number().nullable(),
    })
    .default({ kind: 'attached', path: '', frozen_at: null }),
  /** Stored on the tree root only: a child track shows `null` here even when its root carries one. */
  claude_permissions_policy: claudePermissionsScopeSchema.nullable().default(null),
  created_at: z.number(),
  updated_at: z.number(),
});

export const trackSchema = z.preprocess(
  normalizeLegacyTemplateKeys,
  trackObjectSchema,
);

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
  worker_session_id: z.string(),
  kind: runtimeKindSchema,
  status: workerSessionStateSchema,
  // Older servers and persisted Card event snapshots do not carry activity time.
  updated_at_ms: z.number().optional(),
  provider: agentProviderSchema.optional(),
  terminal_id: z.string().optional(),
  thread_id: z.string().optional(),
  session_id: z.string().optional(),
  source: z.string().optional(),
  thread_status: z.string().optional(),
  // Persisted Card snapshots and cards without a completed turn omit it.
  last_turn_completed_ms: z.number().optional(),
});
export type CardRuntimeView = z.infer<typeof cardRuntimeViewSchema>;

/** `model::Card` — card row. `payload` is opaque `serde_json::Value`. */
export const cardSchema = z.object({
  id: z.string(),
  track_id: z.string(),
  kind: z.string(),
  // Option<String> is omitted (rather than serialized as null) on the wire.
  title: z.string().optional(),
  sort: z.number(),
  // serde_json::Value on the wire: arbitrary JSON. Kernel never inspects.
  payload: z.unknown(),
  runtime: cardRuntimeViewSchema.optional(),
  // Wire payloads from older servers may omit the field; `.default(true)` mirrors the kernel default.
  deletable: z.boolean().default(true),
  created_at: z.number(),
  updated_at: z.number(),
});

/** `model::Overlay` — plugin overlay row. `payload` is opaque JSON. */
export const overlaySchema = z.object({
  id: z.string(),
  plugin_id: z.string(),
  // Documented as "track" | "card" but kept open for forward-compat.
  entity_kind: z.string(),
  entity_id: z.string(),
  kind: z.string(),
  payload: z.unknown(),
  updated_at: z.number(),
});

export const areaUpdatedSchema = z.object({
  ev: z.literal('area.updated'),
  data: areaSchema,
});

export const areaDeletedSchema = z.object({
  ev: z.literal('area.deleted'),
  data: z.object({ id: z.string() }),
});

export const trackUpdatedSchema = z.object({
  ev: z.literal('track.updated'),
  data: z.preprocess(
    normalizeLegacyTemplateKeys,
    trackObjectSchema.extend({
      agent_message: z.string().optional(),
    }),
  ),
});

export const trackDeletedSchema = z.object({
  ev: z.literal('track.deleted'),
  data: z.object({ id: z.string(), area_id: z.string() }),
});

/** Emitted exactly once per validated `from → to` transition. */
export const trackLifecycleChangedSchema = z.object({
  ev: z.literal('track.lifecycle_changed'),
  data: z.object({
    id: z.string(),
    area_id: z.string(),
    from: trackLifecycleSchema,
    to: trackLifecycleSchema,
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
  data: z.object({ id: z.string(), track_id: z.string() }),
});

export const runtimeStartedSchema = z.object({
  ev: z.literal('worker_session.started'),
  data: z.object({
    worker_session_id: z.string(),
    card_id: z.string(),
    kind: runtimeKindSchema,
    agent_provider: agentProviderSchema.nullable(),
    status: workerSessionStateSchema,
  }),
});

export const runtimeStatusChangedSchema = z.object({
  ev: z.literal('worker_session.status_changed'),
  data: z.object({
    worker_session_id: z.string(),
    card_id: z.string(),
    old_status: workerSessionStateSchema,
    new_status: workerSessionStateSchema,
  }),
});

export const runtimeSupersededSchema = z.object({
  ev: z.literal('worker_session.superseded'),
  data: z.object({
    old_worker_session_id: z.string(),
    new_worker_session_id: z.string(),
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
    worker_session_id: z.string(),
    card_id: z.string(),
    track_id: z.string(),
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
    worker_session_id: z.string(),
    card_id: z.string(),
    track_id: z.string(),
    old_phase: harnessPhaseTagSchema,
    new_phase: harnessPhaseTagSchema,
  }),
});

export const harnessTranscriptClearedSchema = z.object({
  ev: z.literal('harness.transcript.cleared'),
  data: z.object({
    worker_session_id: z.string(),
    card_id: z.string(),
    track_id: z.string(),
    // Nullable, and null is NOT the same as 0: rows written before the telemetry
    // existed carry no measurement. The keys are always present on the wire.
    cleared_item_count: z.number().nullable(),
    cleared_params_bytes: z.number().nullable(),
    card_age_ms_at_clear: z.number().nullable(),
  }),
});

export const harnessUserMessageEnqueuedSchema = z.object({
  ev: z.literal('harness.user_message.enqueued'),
  data: z.object({
    worker_session_id: z.string(),
    card_id: z.string(),
    track_id: z.string(),
    char_count: z.number(),
  }),
});

/**
 * `ActorId`: the three id-less actors are a bare `{kind}` object and the rest carry `id`.
 * Written out rather than collapsed to a looser shape so it stays the same type as the generated one.
 */
export const actorIdSchema = z.union([
  z.object({ kind: z.literal('User') }),
  z.object({ kind: z.literal('Kernel') }),
  z.object({ kind: z.literal('KernelDispatcher') }),
  z.object({ kind: z.literal('Plugin'), id: z.string() }),
  z.object({ kind: z.literal('AiPlanner'), id: z.string() }),
  z.object({ kind: z.literal('AiCodex'), id: z.string() }),
  z.object({ kind: z.literal('AiClaude'), id: z.string() }),
  z.object({ kind: z.literal('AiPlannerSession'), id: z.string() }),
  z.object({ kind: z.literal('AiCodexSession'), id: z.string() }),
  z.object({ kind: z.literal('AiClaudeSession'), id: z.string() }),
]);

/**
 * One addressable entry in a planner card's pending queue changed. `actor` is here because the WS
 * frame carries no envelope actor.
 */
export const harnessQueueChangedSchema = z.object({
  ev: z.literal('harness.queue.changed'),
  data: z.object({
    worker_session_id: z.string(),
    card_id: z.string(),
    track_id: z.string(),
    entry_id: z.string(),
    change: z.union([
      z.literal('edited'),
      z.literal('deleted'),
      z.literal('steered'),
      z.literal('restored'),
      z.literal('dropped'),
    ]),
    actor: actorIdSchema,
  }),
});

/** Structured edit-log companion to `card.updated`. `author_plugin_id` is absent for every author but `'plugin'`. */
export const trackReportEditedSchema = z.object({
  ev: z.literal('track.report_edited'),
  data: z.object({
    track_id: z.string(),
    card_id: z.string(),
    author: z.enum(['planner', 'user', 'assistant', 'kernel', 'plugin']),
    author_plugin_id: z.string().optional(),
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

/** Emitted by the orphan-terminal sweeper; carried so the firehose subscription accepts the frame. */
export const terminalDeletedSchema = z.object({
  ev: z.literal('terminal.deleted'),
  data: z.object({
    id: z.string(),
    card_id: z.string(),
  }),
});

/** `state` matches the Rust `PluginState` enum's `Display`; `last_error` is skipped when `None`. */
export const pluginStateSchema = z.object({
  ev: z.literal('plugin.state'),
  data: z.object({
    id: z.string(),
    state: z.string(),
    last_error: z.string().optional(),
  }),
});

/** Boot-time announcement of a plugin MCP tool exposed as `plugin.<plugin_id>.<tool_name>`. */
export const pluginToolRegisteredSchema = z.object({
  ev: z.literal('plugin.tool.registered'),
  data: z.object({
    plugin_id: z.string(),
    tool_name: z.string(),
  }),
});

/** Passthrough of one codex-CLI hook firing; `kind` is `hook.codex.<event>`, `payload` is the raw codex JSON. */
export const codexHookSchema = z.object({
  ev: z.literal('codex.hook'),
  data: z.object({
    card_id: z.string(),
    kind: z.string(),
    hook_idempotency_key: z.string(),
    payload: z.unknown(),
  }),
});

/** Passthrough of one Claude hook firing; mirrors `codexHookSchema`. */
export const claudeHookSchema = z.object({
  ev: z.literal('claude.hook'),
  data: z.object({
    card_id: z.string(),
    kind: z.string(),
    hook_idempotency_key: z.string(),
    payload: z.unknown(),
  }),
});

/** A card asks the dispatcher to spawn a codex worker card; `context` is opaque and forwarded verbatim. */
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
 * A planner card asks the dispatcher to spawn a terminal worker card; `cwd` is absent when
 * deferring to the track/area default.
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

/** Worker reports task completion; `idempotency_key` echoes the matching `*.worker_requested` key. */
export const taskCompletedSchema = z.object({
  ev: z.literal('task.completed'),
  data: z.object({
    idempotency_key: z.string(),
    result: z.unknown(),
    artifacts: z.array(z.string()),
    agent_message: z.string().optional(),
  }),
});

/** Worker reports task failure; `details` carries optional structured evidence, including terminal PTY output. */
export const taskFailedSchema = z.object({
  ev: z.literal('task.failed'),
  data: z.object({
    idempotency_key: z.string(),
    reason: z.string(),
    details: z.unknown().optional(),
    agent_message: z.string().optional(),
  }),
});

/** Failed execution cleanup settled; this does not grant recovery authority. */
export const taskFilePublicationSettledSchema = z.object({
  ev: z.literal('task.file_publication_settled'),
  data: z.object({ task_id: z.string(), operation_id: z.string() }),
});

export const taskCandidateVerificationSettledSchema = z.object({
  ev: z.literal('task.candidate_verification_settled'),
  data: z.object({ task_id: z.string(), operation_id: z.string() }),
});

/** #1727 S4: how one Git delivery settled — a pinned candidate or the kernel's failure classification. */
export const deliverySettlementSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('candidate'),
    candidate_id: z.string(),
    commit_sha: z.string(),
    base_sha: z.string(),
    base_is_ancestor: z.boolean(),
  }),
  z.object({
    kind: z.literal('failed'),
    code: z.enum(['workspace_missing', 'provenance_mismatch', 'commit_failed', 'unresolved']),
    reason: z.string(),
    retry_allowed: z.boolean(),
  }),
]);

/** The wake disposition the settlement transaction decided once; `deferred_to_gate` is the silent one. */
export const deliveryWakeReasonSchema = z.enum([
  'failed', 'ungated_candidate', 'gate_already_terminal', 'deferred_to_gate',
]);

export const taskGitDeliverySettledSchema = z.object({
  ev: z.literal('task.git_delivery_settled'),
  data: z.object({
    task_id: z.string(),
    idempotency_key: z.string(),
    track_id: z.string(),
    card_id: z.string(),
    delivery_id: z.string(),
    ordinal: z.number(),
    result: deliverySettlementSchema,
    wake_reason: deliveryWakeReasonSchema,
  }),
});

export const taskExecutionSettledSchema = z.object({
  ev: z.literal('task.execution_settled'),
  data: z.object({
    task_id: z.string(),
    operation_id: z.string(),
  }),
});

/** The planner revised the track's task plan; `changed_keys` omits `unchanged` upserts. */
export const planUpdatedSchema = z.object({
  ev: z.literal('plan.updated'),
  data: z.object({
    track_id: z.string(),
    changed_keys: z.array(z.string()),
    agent_message: z.string().optional(),
  }),
});

/** The kernel scheduler claimed a plan task; `idempotency_key` is the task id (`"{track_id}:{key}"`). */
export const taskDispatchedSchema = z.object({
  ev: z.literal('task.dispatched'),
  data: z.object({
    idempotency_key: z.string(),
    kind: z.string(),
    agent_message: z.string().optional(),
  }),
});

// Required here although Rust uses `serde(default)` for historical storage: the WS
// outlet reserializes the Rust Event enum instead of forwarding raw persisted payloads.
export const taskContextFrozenSchema = z.object({
  ev: z.literal('task.context_frozen'),
  data: z.object({
    task_id: z.string(),
    track_id: z.string(),
    task_key: z.string(),
    idempotency_key: z.string(),
    refs: z.array(z.object({
      track_id: z.string(),
      block_id: z.string(),
      rev: z.number(),
      hash: z.string(),
      is_root: z.boolean(),
    })),
    doc_revs: z.record(z.string(), z.number()),
    truncated: z.boolean(),
  }),
});

export const taskContextAdvancedSchema = z.object({
  ev: z.literal('task.context_advanced'),
  data: z.object({
    track_id: z.string(),
    task_key: z.string(),
    task_id: z.string(),
    changed_refs: z.array(z.object({
      track_id: z.string(),
      block_id: z.string(),
      from_rev: z.number(),
      to_rev: z.number(),
      from_hash: z.string(),
      to_hash: z.string(),
    })),
    verdict: z.string(),
    rationale: z.string(),
  }),
});

/** The kernel created an isolated workspace directory for a Codex worker card. */
export const workspaceLeasedSchema = z.object({
  ev: z.literal('workspace.leased'),
  data: z.object({
    track_id: z.string(),
    card_id: z.string(),
    lease_id: z.string(),
    path: z.string(),
  }),
});

/** The kernel released the durable workspace lease after completion, compensation, or boot reclaim. */
export const workspaceReleasedSchema = z.object({
  ev: z.literal('workspace.released'),
  data: z.object({
    track_id: z.string(),
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
  verdict: z.enum(['approved', 'changes_requested']),
});

export const ratifyDecisionSchema = z.enum(['grant', 'deny']);

/** The forge action adapter observed a PR merge and completed the parked operation. */
export const forgePrMergedSchema = z.object({
  ev: z.literal('forge.pr.merged'),
  data: z.object({
    track_id: z.string(),
    subject: forgeMergeSubjectSchema,
    head_sha: z.string(),
    merge_sha: z.string(),
  }),
});

/** The planner recorded one dual-review convergence round for a review subject. */
export const reviewRoundSchema = z.object({
  ev: z.literal('review.round'),
  data: z.object({
    track_id: z.string(),
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
    track_id: z.string(),
    reason: z.string(),
  }),
});

export const ratifyResolvedSchema = z.object({
  ev: z.literal('ratify.resolved'),
  data: z.object({
    track_id: z.string(),
    decision: ratifyDecisionSchema,
  }),
});

/**
 * Anchor is externally tagged: bare `'at_start'` / `'at_end'`, or `{ after_block_id }` (may
 * reference an in-batch `temp:<temp_id>` block).
 */
export const proposalAnchorSchema = z.union([
  z.literal('at_start'),
  z.literal('at_end'),
  z.object({ after_block_id: z.string() }),
]);

export const proposalOpSchema = z.discriminatedUnion('op', [
  z.object({
    op: z.literal('upsert_block'),
    block_id: z.string().optional(),
    temp_id: z.string().optional(),
    kind: z.string(),
    payload: z.unknown(),
    if_rev: z.number().optional(),
    anchor: proposalAnchorSchema.optional(),
  }),
  z.object({
    op: z.literal('move_block'),
    block_id: z.string(),
    if_rev: z.number(),
    anchor: proposalAnchorSchema,
  }),
  z.object({
    op: z.literal('delete_block'),
    block_id: z.string(),
    if_rev: z.number(),
  }),
]);

export const proposalDecisionSchema = z.enum([
  'accepted',
  'rejected',
  'stale',
  'withdrawn',
]);

/** A plugin proposed report edits; adjudication is human (see `proposal.resolved`). */
export const proposalSubmittedSchema = z.object({
  ev: z.literal('proposal.submitted'),
  data: z.object({
    track_id: z.string(),
    proposal_id: z.string(),
    plugin_id: z.string(),
    subject_kind: z.string(),
    base_doc_heads: z.string(),
    ops: z.array(proposalOpSchema),
    note: z.string(),
    idem_key: z.string(),
  }),
});

/** A pending proposal reached a terminal decision; `plugin_id` is the submitter, not the resolver. */
export const proposalResolvedSchema = z.object({
  ev: z.literal('proposal.resolved'),
  data: z.object({
    track_id: z.string(),
    proposal_id: z.string(),
    plugin_id: z.string(),
    decision: proposalDecisionSchema,
  }),
});

export const forgeScanCompletedSchema = z.object({
  ev: z.literal('forge.scan.completed'),
  data: z.object({
    track_id: z.string(),
    overlapping_prs: z.array(z.number()),
  }),
});

export const forgePrOpenedSchema = z.object({
  ev: z.literal('forge.pr.opened'),
  data: z.object({
    track_id: z.string(),
    pr_number: z.number(),
    head_sha: z.string(),
  }),
});

export const forgePrDiffReadSchema = z.object({
  ev: z.literal('forge.pr.diff.read'),
  data: z.object({
    track_id: z.string(),
    pr_number: z.number(),
    base_sha: z.string(),
    head_sha: z.string(),
    artifact_path: z.string(),
  }),
});

export const forgePrChecksSchema = z.object({
  ev: z.literal('forge.pr.checks'),
  data: z.object({
    track_id: z.string(),
    pr_number: z.number(),
    conclusion: z.string(),
  }),
});

export const forgeIssueReadSchema = z.object({
  ev: z.literal('forge.issue.read'),
  data: z.object({
    track_id: z.string(),
    issue_number: z.number(),
    artifact_path: z.string(),
  }),
});

export const forgeIssueClosedSchema = z.object({
  ev: z.literal('forge.issue.closed'),
  data: z.object({
    track_id: z.string(),
    issue_number: z.number(),
  }),
});

export const worktreeProvisionedSchema = z.object({
  ev: z.literal('worktree.provisioned'),
  data: z.object({
    track_id: z.string(),
    card_id: z.string(),
    path: z.string(),
  }),
});

export const worktreeCommittedSchema = z.object({
  ev: z.literal('worktree.committed'),
  data: z.object({
    track_id: z.string(),
    card_id: z.string(),
    commit_sha: z.string(),
    branch: z.string(),
    /* #1727 S4: present only on kernel deliveries; legacy auto-commits carry neither. */
    delivery_id: z.string().optional(),
    base_is_ancestor: z.boolean().optional(),
  }),
});

export const worktreeRemovedSchema = z.object({
  ev: z.literal('worktree.removed'),
  data: z.object({
    track_id: z.string(),
    card_id: z.string(),
    path: z.string(),
  }),
});

/** #1727 S4 D3.0: which check of one checkout sample failed against the candidate. */
export const mismatchReasonSchema = z.enum(['provenance', 'head', 'dirty']);

/** The lease-provenance observation line (`realpath`, `common_dir`, `registered`) of one sample. */
export const provenanceSampleSchema = z.object({
  realpath: z.string(),
  common_dir: z.string(),
  registered: z.boolean(),
});

/** One D3.0 sample: HEAD, porcelain status lines, provenance. */
export const sampleSchema = z.object({
  head: z.string(),
  dirty: z.array(z.string()),
  provenance: provenanceSampleSchema,
});

/** Where sampling failed; `cwd` is absent only in `prepare`. */
export const samplePhaseSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('prepare'), reason: z.string() }),
  z.object({ kind: z.literal('finalize'), cwd: z.string(), reason: z.string() }),
  z.object({ kind: z.literal('compensation'), cwd: z.string(), reason: z.string() }),
  z.object({ kind: z.literal('reconciliation'), cwd: z.string(), last_error: z.string() }),
]);

/** How the checkout was compared to the candidate: refused before any step, verified before and after, or unsampled. */
export const verifyTargetEvidenceSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('refused'),
    cwd: z.string(),
    before: sampleSchema,
    reasons: z.array(mismatchReasonSchema),
  }),
  z.object({
    kind: z.literal('verified'),
    cwd: z.string(),
    before: sampleSchema,
    after: sampleSchema,
    reasons: z.array(mismatchReasonSchema),
  }),
  z.object({ kind: z.literal('unsampled'), phase: samplePhaseSchema }),
]);

/** The delivery state the gate found instead of a candidate; each variant carries only facts that exist. */
export const noCandidateReasonSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('delivery_pending'), delivery_id: z.string() }),
  z.object({ kind: z.literal('delivery_failed'), delivery_id: z.string() }),
  z.object({ kind: z.literal('delivery_abandoned'), delivery_id: z.string() }),
  z.object({ kind: z.literal('no_delivery_row') }),
]);

/** Why a gate checked nothing against a candidate. */
export const unboundReasonSchema = z.enum([
  'legacy_lease', 'legacy_frozen', 'legacy_verdict', 'terminal',
]);

/** #1727 S4 D3: what a gate verdict was checked against. */
export const verifyTargetSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('candidate'),
    candidate_id: z.string(),
    commit_sha: z.string(),
    lease_id: z.string(),
    evidence: verifyTargetEvidenceSchema,
  }),
  z.object({ kind: z.literal('no_candidate'), reason: noCandidateReasonSchema }),
  z.object({ kind: z.literal('unbound'), reason: unboundReasonSchema }),
]);

/**
 * One `task-verify` attempt finished; `failing_step` / `exit_code` are absent for verdicts that don't carry them.
 * `status_detail` / `target` (#1727 S4 slice 4) are absent on a passed verdict and on pre-slice-4 producers.
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
    status_detail: z.string().optional(),
    target: verifyTargetSchema.optional(),
  }),
});

/** The event's home scope in the area → track → card hierarchy; `System` is the catch-all. */
export const eventScopeSchema = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('System') }),
  z.object({ kind: z.literal('Area'), id: z.object({ area: z.string() }) }),
  z.object({
    kind: z.literal('Track'),
    id: z.object({ track: z.string(), area: z.string() }),
  }),
  z.object({
    kind: z.literal('Card'),
    id: z.object({ card: z.string(), track: z.string(), area: z.string() }),
  }),
]);

export type EventScope = z.infer<typeof eventScopeSchema>;

/** Keep this 1:1 with `event::Event` in calm-server. */
export const wireEventSchema = z.discriminatedUnion('ev', [
  areaUpdatedSchema,
  areaDeletedSchema,
  trackUpdatedSchema,
  trackDeletedSchema,
  trackLifecycleChangedSchema,
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
  harnessQueueChangedSchema,
  trackReportEditedSchema,
  overlaySetSchema,
  overlayDeletedSchema,
  terminalDeletedSchema,
  pluginStateSchema,
  pluginToolRegisteredSchema,
  codexHookSchema,
  claudeHookSchema,
  codexWorkerRequestedSchema,
  terminalWorkerRequestedSchema,
  taskCompletedSchema,
  taskFailedSchema,
  taskExecutionSettledSchema,
  taskFilePublicationSettledSchema,
  taskCandidateVerificationSettledSchema,
  taskGitDeliverySettledSchema,
  planUpdatedSchema,
  taskDispatchedSchema,
  taskContextFrozenSchema,
  taskContextAdvancedSchema,
  workspaceLeasedSchema,
  workspaceReleasedSchema,
  forgePrMergedSchema,
  reviewRoundSchema,
  ratifyRequestedSchema,
  ratifyResolvedSchema,
  proposalSubmittedSchema,
  proposalResolvedSchema,
  forgeScanCompletedSchema,
  forgePrOpenedSchema,
  forgePrDiffReadSchema,
  forgePrChecksSchema,
  forgeIssueReadSchema,
  forgeIssueClosedSchema,
  worktreeProvisionedSchema,
  worktreeCommittedSchema,
  worktreeRemovedSchema,
  taskGateResultSchema,
]);

export type Area = z.infer<typeof areaSchema>;
export type Track = z.infer<typeof trackSchema>;
export type Card = z.infer<typeof cardSchema>;
export type Overlay = z.infer<typeof overlaySchema>;

export type AreaUpdatedEvent = z.infer<typeof areaUpdatedSchema>;
export type AreaDeletedEvent = z.infer<typeof areaDeletedSchema>;
export type TrackUpdatedEvent = z.infer<typeof trackUpdatedSchema>;
export type TrackDeletedEvent = z.infer<typeof trackDeletedSchema>;
export type TrackLifecycleChangedEvent = z.infer<typeof trackLifecycleChangedSchema>;
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
export type TrackReportEditedEvent = z.infer<typeof trackReportEditedSchema>;
export type OverlaySetEvent = z.infer<typeof overlaySetSchema>;
export type OverlayDeletedEvent = z.infer<typeof overlayDeletedSchema>;
export type TerminalDeletedEvent = z.infer<typeof terminalDeletedSchema>;
export type PluginStateEvent = z.infer<typeof pluginStateSchema>;
export type PluginToolRegisteredEvent = z.infer<typeof pluginToolRegisteredSchema>;
export type CodexHookEvent = z.infer<typeof codexHookSchema>;
export type ClaudeHookEvent = z.infer<typeof claudeHookSchema>;
export type CodexWorkerRequestedEvent = z.infer<typeof codexWorkerRequestedSchema>;
export type TerminalWorkerRequestedEvent = z.infer<typeof terminalWorkerRequestedSchema>;
export type TaskCompletedEvent = z.infer<typeof taskCompletedSchema>;
export type TaskFailedEvent = z.infer<typeof taskFailedSchema>;
export type PlanUpdatedEvent = z.infer<typeof planUpdatedSchema>;
export type TaskDispatchedEvent = z.infer<typeof taskDispatchedSchema>;
export type TaskContextFrozenEvent = z.infer<typeof taskContextFrozenSchema>;
export type TaskContextAdvancedEvent = z.infer<typeof taskContextAdvancedSchema>;
export type WorkspaceLeasedEvent = z.infer<typeof workspaceLeasedSchema>;
export type WorkspaceReleasedEvent = z.infer<typeof workspaceReleasedSchema>;
export type ForgePrMergedEvent = z.infer<typeof forgePrMergedSchema>;
export type ReviewRoundEvent = z.infer<typeof reviewRoundSchema>;
export type RatifyRequestedEvent = z.infer<typeof ratifyRequestedSchema>;
export type RatifyResolvedEvent = z.infer<typeof ratifyResolvedSchema>;
export type ProposalSubmittedEvent = z.infer<typeof proposalSubmittedSchema>;
export type ProposalResolvedEvent = z.infer<typeof proposalResolvedSchema>;
export type ForgeScanCompletedEvent = z.infer<typeof forgeScanCompletedSchema>;
export type ForgePrOpenedEvent = z.infer<typeof forgePrOpenedSchema>;
export type ForgePrDiffReadEvent = z.infer<typeof forgePrDiffReadSchema>;
export type ForgePrChecksEvent = z.infer<typeof forgePrChecksSchema>;
export type ForgeIssueReadEvent = z.infer<typeof forgeIssueReadSchema>;
export type ForgeIssueClosedEvent = z.infer<typeof forgeIssueClosedSchema>;
export type WorktreeProvisionedEvent = z.infer<typeof worktreeProvisionedSchema>;
export type WorktreeCommittedEvent = z.infer<typeof worktreeCommittedSchema>;
export type WorktreeRemovedEvent = z.infer<typeof worktreeRemovedSchema>;
export type TaskGateResultEvent = z.infer<typeof taskGateResultSchema>;
export type TaskGitDeliverySettledEvent = z.infer<typeof taskGitDeliverySettledSchema>;

export type WireEvent = z.infer<typeof wireEventSchema>;

export type WireEventDecodeResult =
  | Readonly<{ status: 'ready'; value: WireEvent }>
  | Readonly<{ status: 'failed'; error: ApiDecodeFailure }>;

/** Unknown or malformed frames are returned as data so callers can log and skip dispatch. */
export function decodeWireEvent(input: unknown): WireEventDecodeResult {
  const parsed = wireEventSchema.safeParse(input);
  if (!parsed.success) {
    return {
      status: 'failed',
      error: { kind: 'decode', message: 'Wire event did not match its schema', cause: parsed.error },
    };
  }
  return { status: 'ready', value: parsed.data };
}
