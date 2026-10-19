import { expect, it } from 'vitest';
import { managedProviderProblem, managedProvidersOperation } from './agent-providers.js';

const READY = Object.freeze({
  availability: { provider: 'claude' as const, status: 'ready' as const, reason: null, checked_at_ms: 1, authentication_notice: null },
  capabilities: { managed_conversation: true, steer: false, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: true },
});

it('decodes registered managed capabilities and preserves availability notices', () => {
  const operation = managedProvidersOperation(true);
  expect(operation.path).toBe('/api/agent-providers/managed?refresh=true');
  expect(operation.responseSchema.parse([READY])).toEqual([READY]);
  expect(managedProviderProblem([READY], 'claude')).toBeNull();
  const unavailable = { ...READY, availability: { ...READY.availability, status: 'unavailable', reason: 'Sign in first', authentication_notice: { kind: 'sign_in_required', text: 'Authenticate on this server', revision: 'r1' } } };
  const answers = operation.responseSchema.parse([unavailable]);
  expect(answers[0].availability.authentication_notice?.text).toBe('Authenticate on this server');
  expect(managedProviderProblem(answers, 'claude')).toBe('Sign in first');
});

it('rejects unknown or incomplete capabilities without supplying a backend', () => {
  const schema = managedProvidersOperation(false).responseSchema;
  expect(schema.safeParse([{ ...READY, capabilities: { managed_conversation: true } }]).success).toBe(false);
  expect(schema.safeParse([{ ...READY, availability: { ...READY.availability, provider: 'unknown' } }]).success).toBe(false);
  expect(managedProviderProblem(undefined, 'codex')).toMatch(/unavailable/);
  expect(managedProviderProblem([READY], 'codex')).toMatch(/unavailable/);
  expect(managedProviderProblem([{ ...READY, capabilities: { ...READY.capabilities, managed_conversation: false } }], 'claude')).toMatch(/does not support/);
});


it('uses backend-declared admission even when account status is unavailable', () => {
  const unavailable = { ...READY, capabilities: { ...READY.capabilities, requires_ready_availability: false }, availability: { ...READY.availability, provider: 'codex' as const, status: 'unavailable' as const, reason: 'Daemon is restarting' } };
  expect(managedProviderProblem(managedProvidersOperation(false).responseSchema.parse([unavailable]), 'codex')).toBeNull();
});


it('requires steer as a boolean without a default', () => {
  const schema = managedProvidersOperation(false).responseSchema;
  const missing: Partial<typeof READY.capabilities> = { ...READY.capabilities };
  delete missing.steer;
  expect(schema.safeParse([{ ...READY, capabilities: missing }]).success).toBe(false);
  expect(schema.safeParse([{ ...READY, capabilities: { ...READY.capabilities, steer: 'true' } }]).success).toBe(false);
});
