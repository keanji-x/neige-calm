import { describe, expect, expectTypeOf, it } from 'vitest';

import { wireEventSchema, type WireEvent } from '../api/schemas.js';
import {
  defineInvalidationPolicies,
  invalidationPlanFor,
  noop,
  taskVerdictInvalidatingKinds,
  TRACK_FILES_DERIVED_KINDS,
  type EventKind,
  type InvalidationPolicy,
  type TrackFilesDerivedKind,
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
    const event = { ev: 'track.report_edited', data: { track_id: 'track-7' } } as Extract<
      WireEvent,
      { ev: 'track.report_edited' }
    >;
    expect(invalidationPlanFor(event)).toEqual({
      invalidate: [
        ['track-files', 'track-7'], ['track-report', 'track-7'], ['track-backlinks'],
        ['today-launchpad'], ['track', 'track-7'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  /*
   * #1253 §6 — the two keys the Today trigger needs, asserted on their own.
   *
   * They are already covered by the literal list above; this states them as a
   * requirement rather than as an incidental part of a snapshot, because the
   * failure they prevent is invisible from inside this layer. A report edit
   * that does not invalidate these two leaves the page unchanged after the
   * button is pressed — with no error, no console warning and no failing
   * golden, since `PolicyMap` is exhaustive over event kinds and not over query
   * keys. Deleting either line from the policy turns this red and nothing else.
   */
  it('refreshes the Today document and its resolve when a report is edited', () => {
    const event = { ev: 'track.report_edited', data: { track_id: 'lp' } } as Extract<
      WireEvent,
      { ev: 'track.report_edited' }
    >;
    const keys = invalidationPlanFor(event).invalidate;
    expect(keys).toContainEqual(['today-launchpad']);
    expect(keys).toContainEqual(['track', 'lp']);
  });

  it('refreshes active task diagnostics when a budget or admission setting changes', () => {
    const event = { ev: 'track.updated', data: { id: 'track-7', area_id: 'area-1' } } as Extract<
      WireEvent,
      { ev: 'track.updated' }
    >;
    expect(invalidationPlanFor(event).invalidate).toContainEqual(['track-report']);
  });

  it('refreshes diagnostics after pending-task cancellation and tree membership changes', () => {
    const planUpdated = {
      ev: 'plan.updated', data: { track_id: 'track-7', agent_message: 'canceled' },
    } as Extract<
      WireEvent,
      { ev: 'plan.updated' }
    >;
    const trackDeleted = { ev: 'track.deleted', data: { id: 'child', area_id: 'area-1' } } as Extract<
      WireEvent,
      { ev: 'track.deleted' }
    >;
    expect(invalidationPlanFor(planUpdated).invalidate).toContainEqual(['track-report', 'track-7']);
    expect(invalidationPlanFor(trackDeleted).invalidate).toContainEqual(['track-report']);
  });

  it('pins track-report invalidation to exactly the task-verdict invalidating kinds', () => {
    const allEventKinds = wireEventSchema.options.map((schema) => schema.shape.ev.value);
    const actual = new Set(allEventKinds.filter((kind) => invalidationPlanFor(
      {
        ev: kind,
        data: kind === 'plan.updated' ? { track_id: 'track-7', agent_message: 'canceled' } : {},
      } as WireEvent,
    ).invalidate.some((key) => key[0] === 'track-report')));
    expect(actual).toEqual(new Set(taskVerdictInvalidatingKinds()));
    expectTypeOf<typeof TRACK_FILES_DERIVED_KINDS[number]>().toEqualTypeOf<TrackFilesDerivedKind>();
  });

  /*
   * The exclusion, asserted from both sides so neither can drift silently.
   *
   * A hook fires roughly twice per tool call per running worker and writes no
   * `tasks` row; `['track-report', …]` is a live query on the whole-document
   * report projection. It must keep its workspace key (the files really did
   * change) and must not have the verdict one.
   */
  it.each(['codex.hook', 'claude.hook'] as const)(
    'invalidates the workspace but never the task verdicts for %s',
    (ev) => {
      const plan = invalidationPlanFor(
        { ev, data: { track_id: 'track-7' } } as unknown as WireEvent,
      );
      expect(plan.invalidate).toEqual([['track-files', 'track-7']]);
      expect(taskVerdictInvalidatingKinds()).not.toContain(ev);
    },
  );

  it('[type-only] rejects a policy map missing any wire event kind', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      // @ts-expect-error -- GATE-APP-028: deleting this whole line must expose the missing-event error.
      defineInvalidationPolicies({ 'area.deleted': noop('fixture') });
    }
    expectTypeOf<EventKind>().toEqualTypeOf<WireEvent['ev']>();
    expectTypeOf<InvalidationPolicy>().toMatchTypeOf<{ type: string }>();
  });
});
