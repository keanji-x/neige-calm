import { describe, expect, it } from 'vitest';

import { plannerTurnDuration, plannerTurnDurationPatch } from './settings.js';

// #2618: the settings owner rejects corrupt storage instead of silently defaulting.
describe('Planner turn duration codec', () => {
  it('reads the absent default and normalizes ASCII integers', () => {
    expect(plannerTurnDuration(undefined)).toEqual({ ok: true, minutes: 30, isDefault: true });
    expect(plannerTurnDuration('001')).toEqual({ ok: true, minutes: 1, isDefault: false });
    expect(plannerTurnDuration('240')).toEqual({ ok: true, minutes: 240, isDefault: false });
  });
  it.each(['', '0', '241', '-1', '1.5', '1e2', ' 30', '３０', 'NaN', '30\n', '30\r', '30\r\n', '+30', '0x1'])('rejects corrupt %s', (value) => {
    expect(plannerTurnDuration(value).ok).toBe(false);
  });
  it('writes only this key and resets explicitly', () => {
    expect(plannerTurnDurationPatch('0030')).toEqual({ planner_max_turn_duration_minutes: '30' });
    expect(plannerTurnDurationPatch(null)).toEqual({ planner_max_turn_duration_minutes: null });
    expect(() => plannerTurnDurationPatch('241')).toThrow();
  });
});
