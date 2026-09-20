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
        ['track-files', 'track-7'], ['track-report'], ['track-backlinks'],
        ['today-launchpad'], ['track', 'track-7'],
      ],
      remove: [],
      writeThrough: [],
    });
  });

  it('refreshes the Today document and its resolve when a report is edited', () => {
    const event = { ev: 'track.report_edited', data: { track_id: 'lp' } } as Extract<
      WireEvent,
      { ev: 'track.report_edited' }
    >;
    const keys = invalidationPlanFor(event).invalidate;
    expect(keys).toContainEqual(['today-launchpad']);
    expect(keys).toContainEqual(['track', 'lp']);
  });

  /* Series keys are per block revision; the event carries no block id, so touching the
   * `track-report-series` prefix would refetch every series block of the track. */
  it('report edit does not refetch unchanged series blocks', () => {
    const event = { ev: 'track.report_edited', data: { track_id: 'track-7' } } as Extract<
      WireEvent,
      { ev: 'track.report_edited' }
    >;
    const plan = invalidationPlanFor(event);
    const touches = [...plan.invalidate, ...plan.remove]
      .filter((key) => key[0] === 'track-report-series');
    expect(touches).toEqual([]);
    // The document itself is still refreshed — that is the path a changed
    // block's new `rev` travels on.
    expect(plan.invalidate).toContainEqual(['track', 'track-7']);
  });

  it('refreshes a card\'s planner run when the card changes', () => {
    const event = {
      ev: 'card.updated',
      data: { id: 'card-9', track_id: 'track-1' },
    } as Extract<WireEvent, { ev: 'card.updated' }>;
    expect(invalidationPlanFor(event).invalidate).toContainEqual(['planner-run', 'card-9']);
  });

  it('refreshes active task diagnostics when a budget or admission setting changes', () => {
    const event = { ev: 'track.updated', data: { id: 'track-7', area_id: 'area-1' } } as Extract<
      WireEvent,
      { ev: 'track.updated' }
    >;
    expect(invalidationPlanFor(event).invalidate).toContainEqual(['track-report']);
  });

  it('refreshes every dependent verdict when a referenced card appears or disappears', () => {
    for (const ev of ['card.added', 'card.deleted'] as const) {
      const event = { ev, data: { track_id: 'target' } } as Extract<WireEvent, { ev: typeof ev }>;
      expect(invalidationPlanFor(event).invalidate).toContainEqual(['track-report']);
    }
  });

  it('refreshes cross-area reference diagnostics after an area disappears', () => {
    const event = { ev: 'area.deleted', data: { id: 'target-area' } } as Extract<
      WireEvent,
      { ev: 'area.deleted' }
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

  /* A hook fires roughly twice per tool call per running worker and writes no `tasks` row. */
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
