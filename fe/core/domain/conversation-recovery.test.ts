import { expect, it } from 'vitest';
import { plannerRunOperation } from './conversation.js';
import { abandonConversationOperation, runRecoverySchema, offersFreshSession, recoveryNotice, recoveryStatusNotice } from './conversation-restart.js';

const RUN = Object.freeze({ card_id: 'card', phase: 'turn_completed', model: null, reasoning_effort: null,
  permission_mode: null, blocked_reason: null, manual_compact_supported: false, compact_outcome: null,
  context_usage_invalidated: false, running_turn: null });

it('requires the nullable recovery field and rejects unknown causes and actions', () => {
  const schema = plannerRunOperation('card').responseSchema;
  expect(schema.safeParse(RUN).success).toBe(false);
  expect(schema.parse({ ...RUN, recovery: null }).recovery).toBeNull();
  for (const cause of ['unknown_outcome', 'wedged'] as const) {
    const recovery = { cause, action: 'restart_session' as const };
    expect(schema.parse({ ...RUN, recovery }).recovery).toEqual(recovery);
    expect(offersFreshSession(recovery)).toBe(true);
    expect(recoveryNotice(recovery)).toContain('fresh model session starts without that context');
  }
  expect(schema.safeParse({ ...RUN, recovery: { cause: 'authentication', action: 'restart_session' } }).success).toBe(false);
  expect(schema.safeParse({ ...RUN, recovery: { cause: 'unknown_outcome', action: 'resend' } }).success).toBe(false);
});

it('offers no recovery merely because a lifecycle phase or a block notice is present', () => {
  for (const phase of ['wedged', 'turn_completed', 'idle']) {
    const run = plannerRunOperation('card').responseSchema.parse({ ...RUN, phase, blocked_reason: 'Unknown outcome', recovery: null });
    expect(offersFreshSession(run.recovery)).toBe(false);
  }
});

it('describes an unknown outcome consistently without calling it stuck', () => {
  const recovery = { cause: 'unknown_outcome', action: 'restart_session' } as const;
  const notice = recoveryStatusNotice(recovery, 'A stale block reason');
  expect(notice).toEqual({ heading: 'Outcome unknown', reason: 'The previous request’s outcome is unknown. It won’t be sent again.' });
  expect(recoveryNotice(recovery)).toContain(notice.reason);
});

it('preserves paused status copy for wedged and non-recovery stalls', () => {
  for (const recovery of [null, { cause: 'wedged', action: 'restart_session' } as const]) {
    expect(recoveryStatusNotice(recovery, 'The stop remains unconfirmed.')).toEqual({ heading: 'Paused', reason: 'The stop remains unconfirmed.' });
    expect(recoveryStatusNotice(recovery, null)).toEqual({ heading: 'Paused', reason: 'This conversation is stuck.' });
  }
});

it('canonical abandonment requires exact declared input and uses its owning endpoint', () => {
  const recovery = runRecoverySchema.parse({ cause: 'unknown_outcome', action: 'abandon_session', request: {
    carrier_id: 'old-worker', submission_id: 'original', attempt_id: 'attempt', revision: '9007199254740993',
  } });
  expect(offersFreshSession(recovery)).toBe(true);
  if (recovery.action !== 'abandon_session') throw new Error('missing canonical declaration');
  const operation = abandonConversationOperation('card/id', recovery);
  expect(operation.path).toBe('/api/cards/card%2Fid/conversation/abandon');
  expect(operation.body).toEqual(recovery.request);
  expect(operation.headers?.['Idempotency-Key']).toBe(abandonConversationOperation('card/id', recovery).headers?.['Idempotency-Key']);
  expect(runRecoverySchema.safeParse({ cause: 'unknown_outcome', action: 'abandon_session' }).success).toBe(false);
});
it('unconfirmed native completion declares a held reason without offering abandonment', () => {
  const recovery = runRecoverySchema.parse({ cause: 'native_completion_pending', action: 'held', reason: 'native_completion_unavailable' });
  expect(offersFreshSession(recovery)).toBe(false);
  expect(recoveryStatusNotice(recovery, 'Waiting for native completion').reason).toContain('Waiting');
});

it('explains the declared worker-bound pending-input refusal without offering a fresh session', () => {
  const recovery = runRecoverySchema.parse({ cause: 'unknown_outcome', action: 'held', reason: 'pending_inputs' });
  expect(offersFreshSession(recovery)).toBe(false);
  expect(recoveryStatusNotice(recovery, null).reason).toContain('queued inputs still belong to the previous session');
});

it('explains an unconfirmed provider launch without offering any action', () => {
  const recovery = runRecoverySchema.parse({ cause: 'unknown_outcome', action: 'held', reason: 'identity_unconfirmed' });
  expect(offersFreshSession(recovery)).toBe(false);
  const notice = recoveryStatusNotice(recovery, null);
  expect(notice.reason).toContain('could not be confirmed');
  expect(notice.reason).not.toContain('fresh session');
});
