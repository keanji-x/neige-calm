import { expect, it } from 'vitest';
import { assistantConversationPayloadSchema } from './conversation.js';
import { plannerSwitchOperation, plannerSwitchResponseSchema, plannerSwitchFailure, PLANNER_SWITCH_COPY, PLANNER_SWITCH_UNCONFIRMED } from './planner-switch.js';

it('declares one CAS operation and requires the receipt revision', () => {
  const body = { target_assistant_card_id: 'a', expected_planner_card_id: 'p', expected_revision: 7 };
  expect(plannerSwitchOperation('a/b', body)).toMatchObject({ method: 'POST', path: '/api/tracks/a%2Fb/planner/switch', body });
  expect(plannerSwitchResponseSchema.safeParse({ track_id: 't', planner_card_id: 'a', demoted_card_id: 'p' }).success).toBe(false);
  expect(plannerSwitchResponseSchema.safeParse({ track_id: 't', planner_card_id: 'a', demoted_card_id: 'p', planner_revision: 8 }).success).toBe(true);
});

it('classifies all eleven protocol refusals with paired copy and blocking card', () => {
  expect(Object.keys(PLANNER_SWITCH_COPY)).toHaveLength(11);
  for (const [code, copy] of Object.entries(PLANNER_SWITCH_COPY)) {
    expect(copy.zh).toMatch(/[\u4e00-\u9fff]/u);
    expect(copy.en).toMatch(/[a-z]/u);
    expect(Object.isFrozen(copy)).toBe(true);
    expect(plannerSwitchFailure({ kind: 'http', status: code === 'planner_user_required' ? 403 : code === 'planner_target_invalid' ? 400 : 409,
      code, message: 'wire copy', body: { card_id: 'blocked' } })).toEqual({ message: copy.en, cardId: 'blocked', unconfirmed: false });
  }
});

it('keeps unknown refusal text but never mistakes uncertain outcomes for refusal', () => {
  expect(plannerSwitchFailure({ kind: 'http', status: 409, code: 'future', message: 'Server reason' }).message).toBe('Server reason');
  for (const failure of [null, { kind: 'transport', message: 'lost' }, { kind: 'decode', message: 'invalid' },
    { kind: 'http', status: 500, code: 'planner_changed', message: 'server' }] as const) {
    expect(plannerSwitchFailure(failure)).toEqual({ message: PLANNER_SWITCH_UNCONFIRMED.en, cardId: null, unconfirmed: true });
  }
});


it('requires a declared Assistant provider without borrowing the Planner display default', () => {
  for (const assistant_provider of [undefined, null, 'future', 1]) {
    expect(assistantConversationPayloadSchema.safeParse({ planner_provider: 'codex', assistant_provider }).success).toBe(false);
  }
  expect(assistantConversationPayloadSchema.parse({ assistant_provider: 'claude', side_source_card_id: 'parent' }))
    .toEqual({ assistant_provider: 'claude', side_source_card_id: 'parent' });
});
