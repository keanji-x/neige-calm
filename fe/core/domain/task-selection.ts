import { z } from 'zod';
import type { ProviderWorkerCapabilities, TaskWorkerSelection } from '../api/generated/wire.js';
import { agentProviderSchema } from '../api/schemas.js';

const selectionSourceSchema = z.enum(['explicit', 'provider_default_reported', 'provider_default_unreported']);
const workerCapabilitiesSchema = z.object({ provider: agentProviderSchema, operation: z.enum(['codex-worker', 'claude-worker', 'acp-worker', 'terminal-worker']),
  message_transport: z.enum(['terminal', 'native_session']), cancel_strategy: z.enum(['provider_stop', 'acp_cancel']),
  resumable: z.boolean(), executor: z.boolean(), validator: z.boolean() }) satisfies z.ZodType<ProviderWorkerCapabilities>;
export const workerSelectionSchema = z.object({
  mechanism: z.string(), provider: agentProviderSchema,
  requested_model: z.string().nullable(), requested_effort: z.string().nullable(),
  effective_model: z.string().nullable(), effective_effort: z.string().nullable(),
  selection_source: z.object({ model: selectionSourceSchema, effort: selectionSourceSchema }).nullable(),
  capabilities: workerCapabilitiesSchema,
  turn_phase: z.enum(['prepared', 'submitting', 'settled', 'unknown']).nullable(),
  cleanup_pending: z.boolean(),
}) satisfies z.ZodType<TaskWorkerSelection>;
export type WorkerSelection = z.infer<typeof workerSelectionSchema>;
export function selectionSourceLabel(source: z.infer<typeof selectionSourceSchema> | undefined): string {
  switch (source) {
    case 'explicit': return 'Explicit';
    case 'provider_default_reported': return 'Provider default';
    case 'provider_default_unreported': return 'Provider default · 未报告';
    default: return '未记录';
  }
}
export function workerSelectionDescription(selection: WorkerSelection): string {
  return `${selection.mechanism} · model ${selection.effective_model ?? '未记录'} (${selectionSourceLabel(selection.selection_source?.model)}) · effort ${selection.effective_effort ?? '未记录'} (${selectionSourceLabel(selection.selection_source?.effort)})`;
}
