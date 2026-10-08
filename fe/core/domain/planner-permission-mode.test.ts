import { describe, expect, it } from 'vitest';

import { plannerRunOperation } from './conversation.js';
import { setPlannerPermissionModeOperation } from './planner-permission-mode.js';

const RUN = {
  card_id: 'card-1', worker_session_id: null, phase: null, model: null, reasoning_effort: null,
  blocked_reason: null, running_turn: null,
};

describe('planner permission mode (#2348)', () => {
  it('writes the one mode the route takes, to the card it names', () => {
    expect(setPlannerPermissionModeOperation('card/1', 'ask')).toMatchObject({
      method: 'PUT', path: '/api/cards/card%2F1/planner/permission-mode', body: { permission_mode: 'ask' },
    });
  });

  it('reads the run\'s mode as required: a mode, `null` for a card that is not a Planner, never absent', () => {
    const { responseSchema } = plannerRunOperation('card-1');
    expect(responseSchema.parse({ ...RUN, permission_mode: 'ask' }).permission_mode).toBe('ask');
    expect(responseSchema.parse({ ...RUN, permission_mode: 'full' }).permission_mode).toBe('full');
    expect(responseSchema.parse({ ...RUN, permission_mode: null }).permission_mode).toBeNull();
    expect(responseSchema.safeParse(RUN).success).toBe(false);
    expect(responseSchema.safeParse({ ...RUN, permission_mode: 'yolo' }).success).toBe(false);
  });
});
