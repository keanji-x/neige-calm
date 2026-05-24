// End-to-end coverage of the wave-lifecycle state machine — issue #269
// P1.
//
// Unit tests in `crates/calm-server/src/wave_lifecycle.rs` already
// cover the (from, to, actor) transition matrix exhaustively and
// `crates/calm-server/tests/terminal_lifecycle.rs` covers the
// `terminal_at` stamp at the DB layer. This suite is the browser-level
// counterpart: real REST writes go through the live kernel, the live
// event bus broadcasts to the browser, and the event-trace ring buffer
// records the resulting `wave.lifecycle_changed` / `wave.updated`
// frames so the assertions prove **the wire contract**, not just the
// in-process state machine.
//
// The spec-only edges (`planning → dispatching → working → reviewing
// → done`) can't be driven by REST under the default `user` actor —
// `validate_transition` rejects them with 403. The replay binary
// exposes `POST /dev/force-wave-lifecycle` (see
// `crates/calm-server/src/bin/replay.rs`) which stamps the edge as
// `ActorId::Kernel` (classified as SpecAgent by
// `wave_lifecycle::actor_kind`) but routes through the exact same
// `write_with_events_typed` pipeline — same validator, same paired
// `WaveLifecycleChanged` + `WaveUpdated` events. The user-driven edges
// (kickoff = `draft → planning`, reopen = `done → planning`) go
// through plain `PATCH /api/waves/{id}` so the event log records them
// as User-driven, matching production attribution.

import { test, expect, type APIRequestContext, type Page } from '@playwright/test';

import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import {
  forceWaveLifecycle,
  getWave,
  patchWaveLifecycle,
  type WaveLifecycle,
  type WaveSnapshot,
} from './helpers/lifecycle';
import { clearEventTrace, getEventTrace, waitForEvent, type TraceEvent } from './helpers/trace';

test.describe('wave lifecycle', () => {
  let coveId: string;
  let waveId: string;

  test.beforeEach(async ({ page, request }) => {
    // Hermetic state — see `helpers/reset.ts`. Without this every
    // assertion below would interact with whatever waves the previous
    // spec left behind in the shared replay binary.
    await resetReplayServer(request);

    // Mint a fresh cove + wave for each test. `createWaveInCove`
    // returns the new wave's id — fresh waves start in `draft`.
    const cove = await createUserCove(request, 'AtlasLifecycle');
    coveId = cove.id;
    const wave = await createWaveInCove(request, coveId, 'Lifecycle test');
    waveId = wave.id;

    // Boot a real browser session with the trace ring buffer enabled —
    // every lifecycle transition below asserts on both the REST
    // response shape *and* the matching WS event landing in
    // `window.__neigeEvents__`.
    await page.goto('/?trace=1');
    // Wait for the buffer to come into existence (the bridge writes it
    // lazily on first WS frame). The newly-minted wave's
    // `wave.updated` event from `createWaveInCove` above will land
    // here; we clear the buffer before driving any per-test transition
    // so trace assertions see only the events under test.
    await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));
    // Give the boot replay a moment to drain the seeded fixture + the
    // freshly-minted cove/wave frames, then clear so each test starts
    // from a clean trace.
    await waitForEvent(page, 'wave.updated');
    await clearEventTrace(page);
  });

  test('full lifecycle Draft -> Planning -> Dispatching -> Working -> Reviewing -> Done', async ({
    page,
    request,
  }) => {
    // Sanity: the freshly-created wave is Draft.
    const initial = await getWave(request, waveId);
    expect(initial.lifecycle).toBe<WaveLifecycle>('draft');
    expect(initial.terminal_at).toBeNull();

    // Step 1: Draft -> Planning. User-driven kickoff (PATCH with no
    // actor header -> ActorId::User). Both User and SpecAgent are
    // allowed to drive this edge; we use PATCH to mirror the "user
    // clicks Start in the UI" production path.
    await runTransition(page, request, waveId, coveId, {
      from: 'draft',
      to: 'planning',
      driver: 'user',
    });

    // Steps 2-5: spec-only edges. Each goes through
    // `/dev/force-wave-lifecycle` (ActorId::Kernel = SpecAgent).
    for (const { from, to } of [
      { from: 'planning' as const, to: 'dispatching' as const },
      { from: 'dispatching' as const, to: 'working' as const },
      { from: 'working' as const, to: 'reviewing' as const },
      { from: 'reviewing' as const, to: 'done' as const },
    ]) {
      await runTransition(page, request, waveId, coveId, { from, to, driver: 'spec' });
    }

    // Final state: Done is terminal, so `terminal_at` is stamped with
    // (roughly) the current time. We bound the stamp at +/-10 minutes
    // around `Date.now()` to absorb clock drift between the test
    // runner and the kernel (in practice they're the same machine,
    // but the kernel may have been booted a few minutes before the
    // test runs).
    const finalSnap = await getWave(request, waveId);
    expect(finalSnap.lifecycle).toBe<WaveLifecycle>('done');
    expect(finalSnap.terminal_at).not.toBeNull();
    const now = Date.now();
    expect(finalSnap.terminal_at!).toBeGreaterThan(now - 10 * 60 * 1000);
    expect(finalSnap.terminal_at!).toBeLessThanOrEqual(now + 10 * 60 * 1000);
  });

  test('reopen Done -> Planning clears terminal_at', async ({ page, request }) => {
    // Drive the wave through the full happy path to land in Done.
    // Reuse the same helper sequence as the first test so the reopen
    // case starts from a realistic terminal-state snapshot rather
    // than a hand-crafted DB poke.
    await patchWaveLifecycle(request, waveId, 'planning');
    await forceWaveLifecycle(request, waveId, 'dispatching');
    await forceWaveLifecycle(request, waveId, 'working');
    await forceWaveLifecycle(request, waveId, 'reviewing');
    await forceWaveLifecycle(request, waveId, 'done');

    const inTerminal = await getWave(request, waveId);
    expect(inTerminal.lifecycle).toBe<WaveLifecycle>('done');
    expect(inTerminal.terminal_at).not.toBeNull();

    // Clear the trace right before the reopen so the event-trace
    // assertion below sees only the reopen frames, not the long happy-
    // path lead-up.
    await clearEventTrace(page);

    // Reopen via plain PATCH — `done -> planning` is user-only per
    // `wave_lifecycle::validate_transition`. The kernel rule is hard:
    // even the Spec Agent can't reopen a terminal wave.
    const reopened = await patchWaveLifecycle(request, waveId, 'planning');
    expect(reopened.lifecycle).toBe<WaveLifecycle>('planning');
    // The whole point of P1's reopen path test: terminal_at MUST be
    // cleared so the calendar window query and the UI's terminal-state
    // badges stop showing the stale Done timestamp.
    expect(reopened.terminal_at).toBeNull();

    // Confirm the REST snapshot agrees with the PATCH response (no
    // racing reset between PATCH commit and GET).
    const fresh = await getWave(request, waveId);
    expect(fresh.lifecycle).toBe<WaveLifecycle>('planning');
    expect(fresh.terminal_at).toBeNull();

    // And the WS event landed: `WaveLifecycleChanged { from: done, to:
    // planning }` fires on reopen exactly like every other edge. Cache
    // invalidation on the frontend hangs off this event, so missing it
    // would leave the UI showing a Done badge against a Planning row.
    const lifecycleEvt = await waitForEvent(page, 'wave.lifecycle_changed');
    expect(extractLifecyclePayload(lifecycleEvt)).toMatchObject({
      id: waveId,
      cove_id: coveId,
      from: 'done',
      to: 'planning',
    });
    // `wave.updated` is the paired emit (cache invalidation key) and
    // carries the cleared `terminal_at`. We assert on the buffer rather
    // than re-polling so a missing emit fails fast.
    const updatedEvts = (await getEventTrace(page)).filter((e) => e.ev === 'wave.updated');
    expect(updatedEvts.length, 'expected paired wave.updated after reopen').toBeGreaterThan(0);
    const lastUpdated = updatedEvts[updatedEvts.length - 1];
    const wave = extractWavePayload(lastUpdated);
    expect(wave.lifecycle).toBe('planning');
    expect(wave.terminal_at).toBeNull();
  });

  test('same-state PATCH is idempotent (no duplicate event)', async ({ page, request }) => {
    // Kick off so we land in a known non-default state — we want to
    // distinguish "the kernel emitted on the no-op" from "the kernel
    // re-emitted the original kickoff" cleanly.
    await patchWaveLifecycle(request, waveId, 'planning');
    // Wait for the kickoff event to land then clear the trace so the
    // assertion below sees only what the same-state PATCH emits.
    await waitForEvent(page, 'wave.lifecycle_changed');
    await clearEventTrace(page);

    // Same-state PATCH - kernel's idempotency shortcut in
    // `update_wave` should return the existing row and emit *neither*
    // a `wave.lifecycle_changed` nor a `wave.updated`.
    const echo = await patchWaveLifecycle(request, waveId, 'planning');
    expect(echo.lifecycle).toBe<WaveLifecycle>('planning');

    // The production `PATCH /api/waves/:id` handler returns only the
    // wave row — no `emitted_events` counter — so we can't assert "no
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

    const trace = await getEventTrace(page);
    const lifecycleEvts = trace.filter((e) => e.ev === 'wave.lifecycle_changed');
    expect(lifecycleEvts, 'idempotent PATCH must not emit wave.lifecycle_changed').toEqual([]);
    const updatedEvts = trace.filter((e) => e.ev === 'wave.updated');
    expect(updatedEvts, 'idempotent PATCH must not emit wave.updated').toEqual([]);
  });

  test('same-state force (kernel) returns emitted_events=0', async ({ page, request }) => {
    // Companion to the user-PATCH idempotency test above. The dev
    // `/dev/force-wave-lifecycle` endpoint returns an `emitted_events`
    // count in its JSON response (see `crates/calm-server/src/bin/replay.rs`
    // — the same-state branch short-circuits with `emitted_events: 0`),
    // which lets us assert idempotency **deterministically** rather than
    // relying on a negative timing window. This catches regressions
    // where the kernel re-emits on a no-op even on a loaded CI runner
    // with WS backpressure, where a 200ms negative window can false-
    // green.
    //
    // First step into a spec-only state via the dev endpoint so we have
    // a known non-default lifecycle the kernel actor is allowed to
    // re-emit (the force endpoint runs as `ActorId::Kernel`, which
    // can't drive `draft → draft` — only spec-reachable states).
    await patchWaveLifecycle(request, waveId, 'planning');
    const stepped = await forceWaveLifecycle(request, waveId, 'dispatching');
    expect(stepped.wave.lifecycle).toBe<WaveLifecycle>('dispatching');
    expect(stepped.emitted_events).toBe(2);
    await waitForEvent(page, 'wave.lifecycle_changed');
    await clearEventTrace(page);

    // Same-state force — the dev endpoint's `from == to` shortcut
    // mirrors the production `update_wave` shortcut and returns the
    // existing row with `emitted_events: 0`. No timing window needed.
    const idempotent = await forceWaveLifecycle(request, waveId, 'dispatching');
    expect(idempotent.wave.lifecycle).toBe<WaveLifecycle>('dispatching');
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
      trace.filter((e) => e.ev === 'wave.lifecycle_changed'),
      'kernel same-state force must not emit wave.lifecycle_changed',
    ).toEqual([]);
    expect(
      trace.filter((e) => e.ev === 'wave.updated'),
      'kernel same-state force must not emit wave.updated',
    ).toEqual([]);
  });
});

// ----- helpers ---------------------------------------------------------

interface TransitionStep {
  from: WaveLifecycle;
  to: WaveLifecycle;
  driver: 'user' | 'spec';
}

/**
 * Drive one lifecycle edge and assert (1) the REST response, (2) the
 * subsequent `GET /api/waves/{id}` snapshot, and (3) the matching
 * `wave.lifecycle_changed` event landing in the trace ring buffer. The
 * trace is cleared at the end so the next call's `waitForEvent` only
 * has to wait for *its* event.
 */
async function runTransition(
  page: Page,
  request: APIRequestContext,
  waveId: string,
  coveId: string,
  step: TransitionStep,
): Promise<void> {
  // Snapshot the pre-transition state to anchor the assertion's
  // before/after expectations.
  const beforeSnap = await getWave(request, waveId);
  expect(
    beforeSnap.lifecycle,
    `pre-transition snapshot lifecycle (expected ${step.from})`,
  ).toBe(step.from);

  if (step.driver === 'user') {
    const after = await patchWaveLifecycle(request, waveId, step.to);
    expect(after.lifecycle, `PATCH response after ${step.from}->${step.to}`).toBe(step.to);
  } else {
    const result = await forceWaveLifecycle(request, waveId, step.to);
    expect(
      result.wave.lifecycle,
      `force-lifecycle response after ${step.from}->${step.to}`,
    ).toBe(step.to);
    expect(
      result.emitted_events,
      `force-lifecycle ${step.from}->${step.to} should emit both WaveLifecycleChanged + WaveUpdated`,
    ).toBe(2);
  }

  // Re-read so the assertion runs against a fresh kernel snapshot.
  const after = await getWave(request, waveId);
  expect(after.lifecycle, `GET /api/waves snapshot after ${step.from}->${step.to}`).toBe(step.to);
  // `terminal_at` correctness across the matrix: stamped on entry to
  // a terminal state, null otherwise. The full-Done assertion in the
  // test body adds the timestamp-window check on top of this.
  const toIsTerminal = step.to === 'done' || step.to === 'canceled' || step.to === 'failed';
  if (toIsTerminal) {
    expect(after.terminal_at, `terminal_at on entry to ${step.to}`).not.toBeNull();
  } else {
    expect(after.terminal_at, `terminal_at on non-terminal ${step.to}`).toBeNull();
  }

  // Browser-side event assertion. The `wave.lifecycle_changed` payload
  // shape mirrors `Event::WaveLifecycleChanged` in
  // `crates/calm-server/src/event.rs` — `{ id, cove_id, from, to }`.
  const evt = await waitForEvent(page, 'wave.lifecycle_changed');
  expect(extractLifecyclePayload(evt)).toMatchObject({
    id: waveId,
    cove_id: coveId,
    from: step.from,
    to: step.to,
  });
  // Clear so the next call's waitForEvent sees only its own event.
  await clearEventTrace(page);
}

/** `wave.lifecycle_changed` events arrive on the bridge with the
 *  envelope shape `{ ev, data, id, eventVersion, ts }` where `data`
 *  carries the typed payload. Narrow it to the variant we need. */
function extractLifecyclePayload(evt: TraceEvent): {
  id: string;
  cove_id: string;
  from: string;
  to: string;
} {
  const data = (evt.data ?? {}) as { id?: string; cove_id?: string; from?: string; to?: string };
  if (!data.id || !data.cove_id || !data.from || !data.to) {
    throw new Error(
      `extractLifecyclePayload: missing fields on event ${JSON.stringify(evt)}`,
    );
  }
  return { id: data.id, cove_id: data.cove_id, from: data.from, to: data.to };
}

function extractWavePayload(evt: TraceEvent): WaveSnapshot {
  // `wave.updated` carries the full wave row as `data`. Mirrors the
  // defensive shape check in `extractLifecyclePayload` above — if the
  // wire shape drifts (e.g. snake → camel rename), `wave.lifecycle`
  // would silently be `undefined` and downstream `.toBe('planning')`
  // would surface a confusing "received undefined" error rather than
  // pointing at the wire-format regression. Throw a clear error here
  // so the failure mode is obvious.
  const data = (evt.data ?? {}) as Partial<WaveSnapshot>;
  if (!data.id || !data.cove_id || !data.lifecycle) {
    throw new Error(
      `extractWavePayload: missing fields (id/cove_id/lifecycle) on event ${JSON.stringify(evt)}`,
    );
  }
  return data as WaveSnapshot;
}
