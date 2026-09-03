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

export const waveFsCardRoleSchema = z.enum([
  'worker',
  'spec',
  'reportcard',
  'assistant',
]);

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
/**
 * #1147 S1 — `Wave.workspace`. The `wave.json` snapshot schema is `.strict()`,
 * so an undeclared key here does not get stripped, it throws: the viewer would
 * stop rendering every wave snapshot written after the field landed.
 */
export const waveFsWaveWorkspaceSchema = z
  .object({
    kind: z.enum(['managed', 'attached']),
    path: z.string(),
    frozen_at: z.number().nullable(),
  })
  .strict();

/**
 * #1209 PR-2 — one-way read compatibility for the pre-rename wave keys.
 *
 * This reader is the most dangerous of the three: its input is `wave.json` /
 * FS-snapshot files **already written to disk**, which spell the template
 * fields `workflow_id` / `workflow_input`. Both new fields carry
 * `.default(null)`, so renaming the keys and nothing else would have made every
 * existing snapshot hydrate as `template_id: null` without a single error —
 * fail-open, exactly what the compatibility read exists to prevent. (And the
 * schema is `.strict()`, so leaving the old key in place would instead reject
 * the snapshot outright.)
 *
 * Deliberately a preprocess step and NOT an optional field on the schema:
 * making the old key part of the shape would give it two spellings again. The
 * old keys are dropped, and only copied over when the new key is absent.
 *
 * Each of the three zod readers in this repo carries its own copy of this
 * function on purpose. A shared helper would make "the third reader was never
 * wired up" a green regression.
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

const waveFsWaveObjectSchema = z.object({
  id: z.string(),
  area_id: z.string(),
  title: z.string(),
  sort: z.number(),
  archived_at: z.number().nullable(),
  pinned_at: z.number().nullable(),
  lifecycle: waveFsWaveLifecycleSchema,
  cwd: z.string(),
  template_id: z.string().nullable().default(null),
  plugin_scope: z.string().nullable().default(null),
  purpose: z.string().nullable().default(null),
  /**
   * Issue #891 — opaque bound-workflow input JSON; `z.unknown()` mirrors the
   * `#[ts(type = "unknown")]` override on the Rust side. Legacy wave.json
   * snapshots without the key hydrate as `null` (same as `template_id`).
   */
  template_input: z.unknown().default(null),
  terminal_at: z.number().nullable(),
  /**
   * Defaulted so `wave.json` snapshots written before #1147 keep parsing —
   * mirrors `#[serde(default)]` on `Wave.workspace` and the DB defaults in
   * migration 0077.
   */
  workspace: waveFsWaveWorkspaceSchema.default({
    kind: 'attached',
    path: '',
    frozen_at: null,
  }),
  created_at: z.number(),
  updated_at: z.number(),
}).strict();

export const waveFsWaveSchema = z.preprocess(
  normalizeLegacyTemplateKeys,
  waveFsWaveObjectSchema,
);
