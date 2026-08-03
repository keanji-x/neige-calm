import { describe, expect, expectTypeOf, it } from 'vitest';

import type { WireEvent } from '../api/schemas.js';
import {
  defineInvalidationPolicies,
  invalidationPlanFor,
  noop,
  type EventKind,
  type InvalidationPolicy,
} from './invalidation-plan.js';

describe('invalidation plan contract', () => {
  it('requires a non-empty reason for explicit no-op policies', () => {
    expect(noop('consumed directly by its card subscriber')).toEqual({
      type: 'noop',
      reason: 'consumed directly by its card subscriber',
    });
    expect(() => noop('')).toThrow(TypeError);
  });

  it('pins ordinary query-key literals independently from production definitions', () => {
    const event = { ev: 'wave.report_edited', data: { wave_id: 'wave-7' } } as Extract<
      WireEvent,
      { ev: 'wave.report_edited' }
    >;
    expect(invalidationPlanFor(event)).toEqual({
      invalidate: [['wave-files', 'wave-7'], ['wave-backlinks']],
      remove: [],
      writeThrough: [],
    });
  });

  it('[type-only] rejects a policy map missing any wire event kind', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- GATE-APP-028: deleting this whole line must expose the missing-event error.
      defineInvalidationPolicies({ 'cove.deleted': noop('fixture') });
    }
    expectTypeOf<EventKind>().toEqualTypeOf<WireEvent['ev']>();
    expectTypeOf<InvalidationPolicy>().toMatchTypeOf<{ type: string }>();
  });
});
