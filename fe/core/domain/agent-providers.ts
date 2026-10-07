// Whether each Planner provider can run on this server right now, and why not (#1817).

import { z } from 'zod';

import { agentProviderSchema, type AgentProvider } from '../api/schemas.js';
import type { ApiOperation } from '../api/types.js';
import type { ProbeText } from './read-failure.js';
import { NotSentError, writeClassOf, writeFailureOf, type FailureTable, type WriteClass, type WriteText } from './failure-class.js';

const checkedAtSchema = z.number();
const authenticationNoticeSchema = z.object({
  kind: z.enum(['sign_in_required', 'refresh_error_reported', 'state_unavailable', 'retry_requested']),
  text: z.string(),
  revision: z.string(),
}).nullable();

/**
 * One provider's answer. `reason` is the server's own sentence with its fix, present exactly when the
 * provider is not `ready`; `not_configured` is a backend this server was not started with.
 */
export const providerAvailabilitySchema = z.discriminatedUnion('status', [
  z.object({ provider: agentProviderSchema, status: z.literal('ready'), reason: z.null(), checked_at_ms: checkedAtSchema, authentication_notice: authenticationNoticeSchema }),
  z.object({ provider: agentProviderSchema, status: z.literal('unavailable'), reason: z.string(), checked_at_ms: checkedAtSchema, authentication_notice: authenticationNoticeSchema }),
  z.object({ provider: agentProviderSchema, status: z.literal('not_configured'), reason: z.string(), checked_at_ms: checkedAtSchema, authentication_notice: authenticationNoticeSchema }),
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

/** A failed Recheck, a read-only probe read by `probeFailureText`; the previous answer stays on screen either way. */
export const RECHECK_TEXT: ProbeText = Object.freeze({ answered: 'The recheck failed.', unfinished: 'The recheck did not complete. Try again.' });

/**
 * Whether track create refuses `provider` while it is not `ready` (#1817), and so whether a picker may
 * offer it then. Claude: refused with the reason. Codex: never refused — its create answers 201 during a
 * daemon outage (#293), and the Planner runs once Codex is back.
 */
export const CREATE_REFUSED_WHEN_UNAVAILABLE: Readonly<Record<AgentProvider, boolean>> = Object.freeze({
  codex: false,
  claude: true,
  opencode: true,
});

/**
 * Said beside the reason of an unavailable provider that create still accepts (only Codex). True on both
 * create paths: without a first message the track answers 201; with one, a daemon outage fails the
 * create (500) before the message is queued, so it goes out on a retry once Codex is back.
 */
export const STILL_CREATES_NOTE = 'A track can still be created, but a first message is only sent once Codex is back.';

/** `provider`'s entry, or `null` when the answer is not in yet or does not name it. */
export function availabilityOf(
  answers: readonly ProviderAvailability[] | undefined,
  provider: AgentProvider,
): ProviderAvailability | null {
  return answers?.find((answer) => answer.provider === provider) ?? null;
}

export const authenticationRetryResponseSchema = z.object({
  status: z.literal('retry_requested'),
  requested_revision: z.string(),
  recovery_notices: z.array(z.object({ card_id: z.string(), track_id: z.string(), title: z.string(), text: z.string() })),
});
export type AuthenticationRetryResponse = z.infer<typeof authenticationRetryResponseSchema>;
/** Owner authorizes the original queued messages to retry after server-side sign-in. */
export function codexAuthenticationRetryOperation(revision: string): ApiOperation<AuthenticationRetryResponse> {
  return { method: 'POST', path: '/api/agent-providers/codex/retry',
    body: { expected_revision: revision }, responseSchema: authenticationRetryResponseSchema };
}

/** Definite refusals precede the durable rearm. A 503 may follow its commit, so it is unknown. */
export const AUTHENTICATION_RETRY_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([400, 403, 409]), is: 'refused' as const })]),
  unauthorized: 'refused', otherwise: 'unknown',
});
export const AUTHENTICATION_RETRY_TEXT: WriteText = Object.freeze({
  refused: 'The retry was not allowed. Read the current provider status before trying again.',
  unknown: 'The retry could not be confirmed. Read the current provider status before trying again.',
});
export function authenticationRetryFailureText(error: unknown): string {
  if (writeFailureOf(error) instanceof NotSentError) return 'Not sent. Reconnect before retrying queued messages.';
  return writeClassOf(error, AUTHENTICATION_RETRY_FAILURES) === 'refused'
    ? AUTHENTICATION_RETRY_TEXT.refused : AUTHENTICATION_RETRY_TEXT.unknown;
}
