import { expect, it } from 'vitest';
import { plannerRunOperation } from './conversation.js';
import { offersFreshSession, recoveryNotice, recoveryStatusNotice } from './conversation-restart.js';

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
