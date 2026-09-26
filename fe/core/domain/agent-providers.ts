// Whether each Planner provider can run on this server right now, and why not (#1817).

import { z } from 'zod';

import { agentProviderSchema, type AgentProvider } from '../api/schemas.js';
import type { ApiOperation } from '../api/types.js';

const checkedAtSchema = z.number();

/**
 * One provider's answer. `reason` is the server's own sentence with its fix, present exactly when the
 * provider is not `ready`; `not_configured` is a backend this server was not started with.
 */
export const providerAvailabilitySchema = z.discriminatedUnion('status', [
  z.object({ provider: agentProviderSchema, status: z.literal('ready'), reason: z.null(), checked_at_ms: checkedAtSchema }),
  z.object({ provider: agentProviderSchema, status: z.literal('unavailable'), reason: z.string(), checked_at_ms: checkedAtSchema }),
  z.object({ provider: agentProviderSchema, status: z.literal('not_configured'), reason: z.string(), checked_at_ms: checkedAtSchema }),
]);

export type ProviderAvailability = z.infer<typeof providerAvailabilitySchema>;

export const agentProvidersSchema = z.array(providerAvailabilitySchema);

/** `GET /api/agent-providers`; `recheck` runs every check again instead of reading the server's 30 s cache. */
export function agentProvidersOperation(recheck: boolean): ApiOperation<ProviderAvailability[]> {
  return {
    method: 'GET',
    path: recheck ? '/api/agent-providers?refresh=true' : '/api/agent-providers',
    responseSchema: agentProvidersSchema,
  };
}

/** `provider`'s entry, or `null` when the answer is not in yet or does not name it. */
export function availabilityOf(
  answers: readonly ProviderAvailability[] | undefined,
  provider: AgentProvider,
): ProviderAvailability | null {
  return answers?.find((answer) => answer.provider === provider) ?? null;
}
