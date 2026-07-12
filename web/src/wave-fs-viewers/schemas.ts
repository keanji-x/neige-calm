import { z } from 'zod';

export const waveFsWaveLifecycleSchema = z.enum([
  'draft',
  'planning',
  'dispatching',
  'working',
  'blocked',
  'reviewing',
  'done',
  'canceled',
  'failed',
]);

export const waveFsRunStatusSchema = z.enum([
  'completed',
  'failed',
  'running',
  'requested',
  'unknown',
]);

export const agentProviderSchema = z.enum(['codex', 'claude']);

export const workerSessionStateSchema = z.enum([
  'starting',
  'running',
  'idle',
  'turn_pending',
  'failed',
  'exited',
  'superseded',
]);

export const runtimeKindSchema = z.enum([
  'terminal',
  'codex',
  'claude',
  'shared-spec',
]);

export const waveFsCardRoleSchema = z.enum(['worker', 'spec', 'reportcard']);

export const waveFsCardMetaSchema = z.object({
  created_at: z.number(),
  deletable: z.boolean(),
  id: z.string(),
  kind: z.string(),
  role: waveFsCardRoleSchema,
  sort: z.number(),
  updated_at: z.number(),
});

export const waveFsRunVerdictSummarySchema = z.object({
  at: z.number(),
  status: z.string(),
});

export const waveFsRunVerdictSchema = z.object({
  at: z.number(),
  reason: z.string().nullable(),
  status: z.string(),
});

export const waveFsRunEventRefSchema = z.object({
  created_at: z.number(),
  event_id: z.number(),
  kind: z.string(),
  payload: z.unknown(),
});

export const waveFsRunEventsSchema = z.object({
  completed: waveFsRunEventRefSchema.nullable(),
  failed: waveFsRunEventRefSchema.nullable(),
  requested: waveFsRunEventRefSchema.nullable(),
  verdict: waveFsRunEventRefSchema.nullable(),
});

export const waveFsRunIndexEntrySchema = z.object({
  finished_at: z.number().nullable(),
  idempotency_key: z.string(),
  kind: z.string(),
  requested_at: z.number().nullable(),
  status: waveFsRunStatusSchema,
  verdict: waveFsRunVerdictSummarySchema.nullable(),
  worker_card_id: z.string().nullable(),
});

export const waveFsRunDetailSchema = z.object({
  events: waveFsRunEventsSchema,
  finished_at: z.number().nullable(),
  idempotency_key: z.string(),
  kind: z.string(),
  requested_at: z.number().nullable(),
  status: waveFsRunStatusSchema,
  verdict: waveFsRunVerdictSchema.nullable(),
  worker_card_id: z.string().nullable(),
  worker_card_payload: z.unknown().nullable(),
});

export const waveFsHookEventSchema = z.object({
  created_at: z.number(),
  event_id: z.number(),
  hook_kind: z.string(),
  kind: z.string(),
  payload: z.unknown(),
});

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

export const waveFsCardsIndexSchema = z.array(waveFsCardMetaSchema);
export const waveFsHookEventsSchema = z.array(waveFsHookEventSchema);
export const waveFsRunsIndexSchema = z.array(waveFsRunIndexEntrySchema);
export const cardRuntimeSchema = z.union([cardRuntimeViewSchema, z.null()]);
export const waveFsWaveSchema = z.object({
  id: z.string(),
  cove_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
  pinned_at: z.number().nullable(),
  lifecycle: waveFsWaveLifecycleSchema,
  cwd: z.string(),
  workflow_id: z.string().nullable().default(null),
  purpose: z.string().nullable(),
  /**
   * Issue #891 — opaque bound-workflow input JSON; `z.unknown()` mirrors the
   * `#[ts(type = "unknown")]` override on the Rust side. Legacy wave.json
   * snapshots without the key hydrate as `null` (same as `workflow_id`).
   */
  workflow_input: z.unknown().default(null),
  terminal_at: z.number().nullable(),
  created_at: z.number(),
  updated_at: z.number(),
}).strict();
