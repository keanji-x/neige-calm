// End-to-end coverage of the track-lifecycle state machine — issue #269
// P1.
//
// Unit tests in `crates/calm-server/src/track_lifecycle.rs` already
// cover the (from, to, actor) transition matrix exhaustively and
// `crates/calm-server/tests/terminal_lifecycle.rs` covers the
// `terminal_at` stamp at the DB layer. This suite is the browser-level
// counterpart: real REST writes go through the live kernel, the live
// event bus broadcasts to the browser, and the event-trace ring buffer
// records the resulting `track.lifecycle_changed` / `track.updated`
// frames so the assertions prove **the wire contract**, not just the
// in-process state machine.
//
// The planner-only edges (`planning → dispatching → working → reviewing
// → done`) can't be driven by REST under the default `user` actor —
// `validate_transition` rejects them with 403. The replay binary
// exposes `POST /dev/force-track-lifecycle` (see
// `crates/calm-server/src/bin/replay.rs`) which stamps the edge as
// `ActorId::Kernel` (classified as PlannerAgent by
// `track_lifecycle::actor_kind`) but routes through the exact same
// `write_with_events_typed` pipeline — same validator, same paired
// `TrackLifecycleChanged` + `TrackUpdated` events. The user-driven edges
// (kickoff = `draft → planning`, reopen = `done → planning`) go
// through plain `PATCH /api/tracks/{id}` so the event log records them
// as User-driven, matching production attribution.

import { test, expect, type APIRequestContext, type Page } from '@playwright/test';

import { createUserArea, createTrackInArea, resetReplayServer } from './helpers/reset';
import {
  forceTrackLifecycle,
  getTrack,
  patchTrackLifecycle,
  type TrackLifecycle,
  type TrackSnapshot,
} from './helpers/lifecycle';
import { clearEventTrace, getEventTrace, waitForEvent, type TraceEvent } from './helpers/trace';

test.describe('track lifecycle', () => {
  let areaId: string;
  let trackId: string;

  test.beforeEach(async ({ page, request }) => {
    // Hermetic state — see `helpers/reset.ts`. Without this every
    // assertion below would interact with whatever tracks the previous
    // test left behind in the shared replay binary.
    await resetReplayServer(request);

    // Mint a fresh area + track for each test. `createTrackInArea`
    // returns the new track's id — fresh tracks start in `draft`.
    const area = await createUserArea(request, 'AtlasLifecycle');
    areaId = area.id;
    const track = await createTrackInArea(request, areaId, 'Lifecycle test');
    trackId = track.id;

    // Boot a real browser session with the trace ring buffer enabled —
    // every lifecycle transition below asserts on both the REST
    // response shape *and* the matching WS event landing in
    // `window.__neigeEvents__`.
    await page.goto('/?trace=1');
    // Wait for the buffer to come into existence (the bridge writes it
    // lazily on first WS frame). The newly-minted track's
    // `track.updated` event from `createTrackInArea` above will land
    // here; we clear the buffer before driving any per-test transition
    // so trace assertions see only the events under test.
    await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));
    // Give the boot replay a moment to drain the seeded fixture + the
    // freshly-minted area/track frames, then clear so each test starts
    // from a clean trace.
    await waitForEvent(page, 'track.updated');
    await clearEventTrace(page);
  });

  test('full lifecycle Draft -> Planning -> Dispatching -> Working -> Reviewing -> Done', async ({
    page,
    request,
  }) => {
    // Sanity: the freshly-created track is Draft.
    const initial = await getTrack(request, trackId);
    expect(initial.lifecycle).toBe<TrackLifecycle>('draft');
    expect(initial.terminal_at).toBeNull();

    // Step 1: Draft -> Planning. User-driven kickoff (PATCH with no
    // actor header -> ActorId::User). Both User and PlannerAgent are
    // allowed to drive this edge; we use PATCH to mirror the "user
    // clicks Start in the UI" production path.
    await runTransition(page, request, trackId, areaId, {
      from: 'draft',
      to: 'planning',
      driver: 'user',
    });

    // Steps 2-5: planner-only edges. Each goes through
    // `/dev/force-track-lifecycle` (ActorId::Kernel = PlannerAgent).
    for (const { from, to } of [
      { from: 'planning' as const, to: 'dispatching' as const },
      { from: 'dispatching' as const, to: 'working' as const },
      { from: 'working' as const, to: 'reviewing' as const },
      { from: 'reviewing' as const, to: 'done' as const },
    ]) {
      await runTransition(page, request, trackId, areaId, { from, to, driver: 'planner' });
    }

    // Final state: Done is terminal, so `terminal_at` is stamped with
    // (roughly) the current time. We bound the stamp at +/-10 minutes
    // around `Date.now()` to absorb clock drift between the test
    // runner and the kernel (in practice they're the same machine,
    // but the kernel may have been booted a few minutes before the
    // test runs).
    const finalSnap = await getTrack(request, trackId);
    expect(finalSnap.lifecycle).toBe<TrackLifecycle>('done');
    expect(finalSnap.terminal_at).not.toBeNull();
    const now = Date.now();
    expect(finalSnap.terminal_at!).toBeGreaterThan(now - 10 * 60 * 1000);
    expect(finalSnap.terminal_at!).toBeLessThanOrEqual(now + 10 * 60 * 1000);
  });

  test('reopen Done -> Planning clears terminal_at', async ({ page, request }) => {
    // Drive the track through the full happy path to land in Done.
    // Reuse the same helper sequence as the first test so the reopen
    // case starts from a realistic terminal-state snapshot rather
    // than a hand-crafted DB poke.
    await patchTrackLifecycle(request, trackId, 'planning');
    await forceTrackLifecycle(request, trackId, 'dispatching');
    await forceTrackLifecycle(request, trackId, 'working');
    await forceTrackLifecycle(request, trackId, 'reviewing');
    await forceTrackLifecycle(request, trackId, 'done');

    const inTerminal = await getTrack(request, trackId);
    expect(inTerminal.lifecycle).toBe<TrackLifecycle>('done');
    expect(inTerminal.terminal_at).not.toBeNull();

    // Clear the trace right before the reopen so the event-trace
    // assertion below sees only the reopen frames, not the long happy-
    // path lead-up.
    await clearEventTrace(page);

    // Reopen via plain PATCH — `done -> planning` is user-only per
    // `track_lifecycle::validate_transition`. The kernel rule is hard:
    // even the Planner Agent can't reopen a terminal track.
    const reopened = await patchTrackLifecycle(request, trackId, 'planning');
    expect(reopened.lifecycle).toBe<TrackLifecycle>('planning');
    // The whole point of P1's reopen path test: terminal_at MUST be
    // cleared so the calendar window query and the UI's terminal-state
    // badges stop showing the stale Done timestamp.
    expect(reopened.terminal_at).toBeNull();

    // Confirm the REST snapshot agrees with the PATCH response (no
    // racing reset between PATCH commit and GET).
    const fresh = await getTrack(request, trackId);
    expect(fresh.lifecycle).toBe<TrackLifecycle>('planning');
    expect(fresh.terminal_at).toBeNull();

    // And the WS event landed: `TrackLifecycleChanged { from: done, to:
    // planning }` fires on reopen exactly like every other edge. Cache
    // invalidation on the frontend hangs off this event, so missing it
    // would leave the UI showing a Done badge against a Planning row.
    const lifecycleEvt = await waitForEvent(page, 'track.lifecycle_changed');
    expect(extractLifecyclePayload(lifecycleEvt)).toMatchObject({
      id: trackId,
      area_id: areaId,
      from: 'done',
      to: 'planning',
    });
    // `track.updated` is the paired emit (cache invalidation key) and
    // carries the cleared `terminal_at`. We assert on the buffer rather
    // than re-polling so a missing emit fails fast.
    const updatedEvts = (await getEventTrace(page)).filter((e) => e.ev === 'track.updated');
    expect(updatedEvts.length, 'expected paired track.updated after reopen').toBeGreaterThan(0);
    const lastUpdated = updatedEvts[updatedEvts.length - 1];
    const track = extractTrackPayload(lastUpdated);
    expect(track.lifecycle).toBe('planning');
    expect(track.terminal_at).toBeNull();
  });

  test('same-state PATCH is idempotent (no duplicate event)', async ({ page, request }) => {
    // Kick off so we land in a known non-default state — we want to
    // distinguish "the kernel emitted on the no-op" from "the kernel
    // re-emitted the original kickoff" cleanly.
    await patchTrackLifecycle(request, trackId, 'planning');
    // Wait for the kickoff event to land then clear the trace so the
    // assertion below sees only what the same-state PATCH emits.
    await waitForEvent(page, 'track.lifecycle_changed');
    await clearEventTrace(page);

    // Same-state PATCH - kernel's idempotency shortcut in
    // `update_track` should return the existing row and emit *neither*
    // a `track.lifecycle_changed` nor a `track.updated`.
    const echo = await patchTrackLifecycle(request, trackId, 'planning');
    expect(echo.lifecycle).toBe<TrackLifecycle>('planning');

    // The production `PATCH /api/tracks/:id` handler returns only the
    // track row — no `emitted_events` counter — so we can't assert "no
    // event emitted" deterministically via the response. Instead we
    // give any (incorrectly-emitted) event a fair chance to make it
    // through the bus -> WS -> bridge pipeline and then assert the
    // trace ring buffer stayed empty. The 500ms window is wide enough
    // to absorb WS backpressure / scheduling jitter on a loaded CI
    // runner; the deterministic count-based assertion lives in the
    // companion `same-state force (kernel) returns emitted_events=0`
    // test below, which exercises the same idempotency shortcut on the
    // dev endpoint that does expose a counter.
    await page.waitForTimeout(500);

    // Scope the assertion to events for THIS track. The replay binary's
    // default-area bootstrap can emit a late `track.updated` for the
    // unrelated "Today" track that drifts into the 500ms window above on
    // a loaded runner — that's not what idempotency is about. The
    // kernel's idempotency contract is per-track, so filter on the
    // PATCHed track's id and the assertion stays sharp without the
    // cross-track flake.
    const eventTrackId = (e: { data?: unknown }): string | undefined =>
      (e.data as { id?: string } | undefined)?.id;
    const trace = await getEventTrace(page);
    const lifecycleEvts = trace.filter(
      (e) => e.ev === 'track.lifecycle_changed' && eventTrackId(e) === trackId,
    );
    expect(lifecycleEvts, 'idempotent PATCH must not emit track.lifecycle_changed').toEqual([]);
    const updatedEvts = trace.filter(
      (e) => e.ev === 'track.updated' && eventTrackId(e) === trackId,
    );
    expect(updatedEvts, 'idempotent PATCH must not emit track.updated').toEqual([]);
  });

  test('same-state force (kernel) returns emitted_events=0', async ({ page, request }) => {
    // Companion to the user-PATCH idempotency test above. The dev
    // `/dev/force-track-lifecycle` endpoint returns an `emitted_events`
    // count in its JSON response (see `crates/calm-server/src/bin/replay.rs`
    // — the same-state branch short-circuits with `emitted_events: 0`),
    // which lets us assert idempotency **deterministically** rather than
    // relying on a negative timing window. This catches regressions
    // where the kernel re-emits on a no-op even on a loaded CI runner
    // with WS backpressure, where a 200ms negative window can false-
    // green.
    //
    // First step into a planner-only state via the dev endpoint so we have
    // a known non-default lifecycle the kernel actor is allowed to
    // re-emit (the force endpoint runs as `ActorId::Kernel`, which
    // can't drive `draft → draft` — only planner-reachable states).
    await patchTrackLifecycle(request, trackId, 'planning');
    const stepped = await forceTrackLifecycle(request, trackId, 'dispatching');
    expect(stepped.track.lifecycle).toBe<TrackLifecycle>('dispatching');
    expect(stepped.emitted_events).toBe(2);
    await waitForEvent(page, 'track.lifecycle_changed');
    await clearEventTrace(page);

    // Same-state force — the dev endpoint's `from == to` shortcut
    // mirrors the production `update_track` shortcut and returns the
    // existing row with `emitted_events: 0`. No timing window needed.
    const idempotent = await forceTrackLifecycle(request, trackId, 'dispatching');
    expect(idempotent.track.lifecycle).toBe<TrackLifecycle>('dispatching');
    expect(
      idempotent.emitted_events,
      'kernel same-state force must short-circuit without emitting',
    ).toBe(0);

    // Cross-check the trace ring buffer agrees — if the kernel
    // mistakenly emitted *and* still returned 0 (a wire-format bug
    // that would still pass the count assertion above), the trace
    // would catch it. Wait briefly so any rogue event has a chance
    // to land before we assert.
    await page.waitForTimeout(100);
    const trace = await getEventTrace(page);
    expect(
      trace.filter((e) => e.ev === 'track.lifecycle_changed'),
      'kernel same-state force must not emit track.lifecycle_changed',
    ).toEqual([]);
    expect(
      trace.filter((e) => e.ev === 'track.updated'),
      'kernel same-state force must not emit track.updated',
    ).toEqual([]);
  });
});

// ----- helpers ---------------------------------------------------------

interface TransitionStep {
  from: TrackLifecycle;
  to: TrackLifecycle;
  driver: 'user' | 'planner';
}

/**
 * Drive one lifecycle edge and assert (1) the REST response, (2) the
 * subsequent `GET /api/tracks/{id}` snapshot, and (3) the matching
 * `track.lifecycle_changed` event landing in the trace ring buffer. The
 * trace is cleared at the end so the next call's `waitForEvent` only
 * has to wait for *its* event.
 */
async function runTransition(
  page: Page,
  request: APIRequestContext,
  trackId: string,
  areaId: string,
  step: TransitionStep,
): Promise<void> {
  // Snapshot the pre-transition state to anchor the assertion's
  // before/after expectations.
  const beforeSnap = await getTrack(request, trackId);
  expect(
    beforeSnap.lifecycle,
    `pre-transition snapshot lifecycle (expected ${step.from})`,
  ).toBe(step.from);

  if (step.driver === 'user') {
    const after = await patchTrackLifecycle(request, trackId, step.to);
    expect(after.lifecycle, `PATCH response after ${step.from}->${step.to}`).toBe(step.to);
  } else {
    const result = await forceTrackLifecycle(request, trackId, step.to);
    expect(
      result.track.lifecycle,
      `force-lifecycle response after ${step.from}->${step.to}`,
    ).toBe(step.to);
    expect(
      result.emitted_events,
      `force-lifecycle ${step.from}->${step.to} should emit both TrackLifecycleChanged + TrackUpdated`,
    ).toBe(2);
  }

  // Re-read so the assertion runs against a fresh kernel snapshot.
  const after = await getTrack(request, trackId);
  expect(after.lifecycle, `GET /api/tracks snapshot after ${step.from}->${step.to}`).toBe(step.to);
  // `terminal_at` correctness across the matrix: stamped on entry to
  // a terminal state, null otherwise. The full-Done assertion in the
  // test body adds the timestamp-window check on top of this.
  const toIsTerminal = step.to === 'done' || step.to === 'canceled' || step.to === 'failed';
  if (toIsTerminal) {
    expect(after.terminal_at, `terminal_at on entry to ${step.to}`).not.toBeNull();
  } else {
    expect(after.terminal_at, `terminal_at on non-terminal ${step.to}`).toBeNull();
  }

  // Browser-side event assertion. The `track.lifecycle_changed` payload
  // shape mirrors `Event::TrackLifecycleChanged` in
  // `crates/calm-server/src/event.rs` — `{ id, area_id, from, to }`.
  const evt = await waitForEvent(page, 'track.lifecycle_changed');
  expect(extractLifecyclePayload(evt)).toMatchObject({
    id: trackId,
    area_id: areaId,
    from: step.from,
    to: step.to,
  });
  // Clear so the next call's waitForEvent sees only its own event.
  await clearEventTrace(page);
}

/** `track.lifecycle_changed` events arrive on the bridge with the
 *  envelope shape `{ ev, data, id, eventVersion, ts }` where `data`
 *  carries the typed payload. Narrow it to the variant we need. */
function extractLifecyclePayload(evt: TraceEvent): {
  id: string;
  area_id: string;
  from: string;
  to: string;
} {
  const data = (evt.data ?? {}) as { id?: string; area_id?: string; from?: string; to?: string };
  if (!data.id || !data.area_id || !data.from || !data.to) {
    throw new Error(
      `extractLifecyclePayload: missing fields on event ${JSON.stringify(evt)}`,
    );
  }
  return { id: data.id, area_id: data.area_id, from: data.from, to: data.to };
}

function extractTrackPayload(evt: TraceEvent): TrackSnapshot {
  // `track.updated` carries the full track row as `data`. Mirrors the
  // defensive shape check in `extractLifecyclePayload` above — if the
  // wire shape drifts (e.g. snake → camel rename), `track.lifecycle`
  // would silently be `undefined` and downstream `.toBe('planning')`
  // would surface a confusing "received undefined" error rather than
  // pointing at the wire-format regression. Throw a clear error here
  // so the failure mode is obvious.
  const data = (evt.data ?? {}) as Partial<TrackSnapshot>;
  if (!data.id || !data.area_id || !data.lifecycle) {
    throw new Error(
      `extractTrackPayload: missing fields (id/area_id/lifecycle) on event ${JSON.stringify(evt)}`,
    );
  }
  return data as TrackSnapshot;
}
