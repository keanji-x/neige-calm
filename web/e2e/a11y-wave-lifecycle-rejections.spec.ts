// Negative-path E2E coverage for the wave-lifecycle validator —
// follow-up to issue #269 P1 (PR #277 only exercised happy edges).
//
// PR #277's `a11y-wave-lifecycle.spec.ts` pins the user-driven and
// spec-driven *successful* transitions end-to-end. This sibling file
// pins the **rejection** side of the contract: the validator must
// refuse the illegal/unauthorized edges that would otherwise corrupt
// the state machine.
//
// Three sub-cases, all exercised against the same in-memory replay
// kernel via the existing `a11y` Playwright project:
//
//   1. **User attempts a spec-only edge.** With the wave parked in
//      `planning` (a legal user-driven kickoff), a plain
//      `PATCH /api/waves/:id` (no `X-Calm-Actor` → `ActorId::User`)
//      requesting `dispatching` must come back 403. The validator's
//      per-edge `(allow_user, allow_spec) = (false, true)` table
//      entry for `planning → dispatching` collapses to
//      `NotAuthorized` for a `User` actor — production behavior the
//      happy-path suite cannot prove (it always sends `planning →
//      dispatching` via the dev endpoint as `ActorId::Kernel`).
//
//   2. **Kernel attempts a forbidden skip.** Using the dev endpoint
//      `POST /dev/force-wave-lifecycle` (which stamps the transition
//      as `ActorId::Kernel`, classified `SpecAgent` by `actor_kind`),
//      attempt `draft → done`. This isn't an authorization failure —
//      it's a structurally illegal edge that falls through the match
//      arm and surfaces as `TransitionError::IllegalEdge`. The dev
//      handler maps both validator errors to 403; the response body's
//      `error` field disambiguates by carrying the validator's
//      `Display` text, which the assertion below pattern-matches.
//
//   3. **Worker cannot drive any lifecycle edge.** The Worker case is
//      reachable from e2e via the actor header alone: sending
//      `X-Calm-Actor: ai:codex` makes `Actor::to_actor_id` produce
//      `ActorId::AiCodex(CardId::empty())`, which `actor_kind`
//      unconditionally classifies as `Worker` (regardless of cache
//      lookup — that comes later in `role_gate`). The validator's
//      first-pass `if kind == ActorKind::Worker { reject }` short-
//      circuits before any per-edge logic runs, so even the
//      "everyone-allowed" `draft → planning` kickoff comes back 403
//      under a Worker actor.
//
// Rationale for a separate spec file (vs extending
// `a11y-wave-lifecycle.spec.ts`):
//   * The happy-path suite already pushes 360 lines and threads a
//     shared `runTransition` helper that asserts on the WS event
//     trace. The rejection suite needs neither — it asserts on the
//     REST response status + body shape, and never expects an event
//     to land. Keeping the two concerns physically separate makes
//     each file's contract obvious from the first 40 lines.
//   * The describe-block naming convention (`wave lifecycle` vs
//     `wave lifecycle · rejections`) also gives `npx playwright
//     test --grep` a clean handle for running just one side.

import { test, expect, type APIRequestContext } from '@playwright/test';

import { REPLAY_PORT, createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import {
  forceWaveLifecycleRaw,
  patchWaveLifecycle,
  patchWaveLifecycleRaw,
  type WaveLifecycle,
} from './helpers/lifecycle';

/** GET the wave row and return its current `lifecycle` field. Used by
 *  each test below to confirm a rejected transition did NOT mutate the
 *  row (the validator must run before the write). Throws on non-2xx so
 *  a transport / shape regression surfaces in the test that triggered
 *  it. */
async function getLifecycle(request: APIRequestContext, waveId: string): Promise<WaveLifecycle> {
  const res = await request.get(`http://127.0.0.1:${REPLAY_PORT}/api/waves/${waveId}`);
  if (!res.ok()) {
    throw new Error(`getLifecycle(${waveId}): GET → ${res.status()} ${res.statusText()}`);
  }
  const detail = (await res.json()) as { wave: { lifecycle: WaveLifecycle } };
  return detail.wave.lifecycle;
}

test.describe('wave lifecycle · rejections', () => {
  let waveId: string;

  test.beforeEach(async ({ request }) => {
    // Hermetic state — see `helpers/reset.ts`. Without this the
    // rejection-status assertions below could be poisoned by waves
    // an earlier spec parked in some unexpected lifecycle state.
    await resetReplayServer(request);

    // Mint a fresh cove + wave. The wave starts in `draft`; each
    // test below parks it in whatever state its rejection scenario
    // needs before the negative PATCH/force attempt.
    const cove = await createUserCove(request, 'AtlasReject');
    const wave = await createWaveInCove(request, cove.id, 'Rejection test');
    waveId = wave.id;
  });

  test('User PATCH for a spec-only edge (planning → dispatching) is rejected with 403', async ({
    request,
  }) => {
    // Park the wave in `planning` via the legal user-driven kickoff.
    // The kickoff itself is allowed for User (`(true, true)` in the
    // validator's edge table), so this PATCH succeeds; it puts the
    // wave in a state where `dispatching` is the next legal edge —
    // but only for the SpecAgent, not the user.
    const kicked = await patchWaveLifecycle(request, waveId, 'planning');
    expect(kicked.lifecycle).toBe('planning');

    // The rejection itself: PATCH lifecycle: 'dispatching' with no
    // actor header → User. The validator's `(planning, dispatching)`
    // entry is `(allow_user: false, allow_spec: true)`, so the
    // `match kind` block at the end of `validate_transition` returns
    // `NotAuthorized`. `update_wave` wraps that in
    // `CalmError::Forbidden`, which `CalmError::status` maps to 403.
    const res = await patchWaveLifecycleRaw(request, waveId, 'dispatching');
    expect(res.status(), 'User-driven planning → dispatching must be Forbidden').toBe(403);

    // CalmError's IntoResponse body is `{error, code, ...}` (see
    // `crates/calm-server/src/error.rs`). Pin the `code` to
    // `"forbidden"` so a future error-shape refactor that silently
    // drops the discriminator doesn't false-green this test.
    const body = (await res.json()) as { code?: string; error?: string };
    expect(body.code).toBe('forbidden');

    // And the wave's lifecycle has NOT advanced — the validator runs
    // before the write, so the row stays in `planning`.
    expect(await getLifecycle(request, waveId)).toBe('planning');
  });

  test('Kernel force-lifecycle for an illegal skip (draft → done) is rejected with 403', async ({
    request,
  }) => {
    // The wave is in `draft` (fresh from `createWaveInCove` in
    // `beforeEach`). `draft → done` has no entry in the validator's
    // per-edge table, so the catch-all `_ => Err(IllegalEdge)` arm
    // fires — the dev handler maps `validate_transition` errors to
    // 403 regardless of `IllegalEdge` vs `NotAuthorized`.
    const res = await forceWaveLifecycleRaw(request, waveId, 'done');
    expect(res.status(), 'Kernel-driven draft → done is structurally illegal').toBe(403);

    // The dev endpoint's error body shape is
    // `{ok: false, error: "validate_transition: <Display>", from, to}`
    // (see `dev_force_wave_lifecycle` in
    // `crates/calm-server/src/bin/replay.rs`). Pin the discriminator
    // pieces so a future refactor that swaps the validator's Display
    // (or the dev handler's wrapper text) is forced to update this
    // assertion deliberately.
    const body = (await res.json()) as {
      ok?: boolean;
      error?: string;
      from?: string;
      to?: string;
    };
    expect(body.ok).toBe(false);
    expect(body.error ?? '').toMatch(/validate_transition/);
    // The validator's TransitionError::IllegalEdge Display includes
    // both endpoints; assert both so a regression that silently
    // returns a NotAuthorized error (a real semantic change) flips
    // this test rather than slipping through.
    expect(body.error ?? '').toMatch(/draft/i);
    expect(body.error ?? '').toMatch(/done/i);
    expect(body.from).toBe('draft');
    expect(body.to).toBe('done');

    // The wave stays in `draft` — validator runs before the write.
    expect(await getLifecycle(request, waveId)).toBe('draft');
  });

  test('Worker actor (X-Calm-Actor: ai:codex) cannot drive any lifecycle edge', async ({
    request,
  }) => {
    // The wave is in `draft`; `draft → planning` is the universal
    // kickoff edge — `(allow_user, allow_spec) = (true, true)` —
    // i.e. the validator's only `(both-allowed)` row. If even THIS
    // edge rejects under a Worker actor, the Worker short-circuit
    // at the top of `validate_transition` is doing its job.
    //
    // `X-Calm-Actor: ai:codex` is the one Worker-shaped header that
    // the actor middleware accepts today (see `to_actor_id` —
    // `"ai:codex"` maps to `ActorId::AiCodex(CardId::empty())`,
    // which `actor_kind` then classifies as `Worker`). Other
    // `ai:<id>` forms are rejected by the middleware as
    // `BadRequest`, so we use the production-shaped one.
    //
    // Side note on role_gate: the empty CardId won't be in the
    // `card_role_cache`, so a *successful* write would later be
    // rejected by the role-gate's `UnknownCard` arm. That doesn't
    // matter here — the validator fires earlier in `update_wave`
    // (before `write_with_events_typed` even runs), so the 403 we
    // assert on is from the lifecycle validator, not the role gate.
    const res = await patchWaveLifecycleRaw(request, waveId, 'planning', {
      actorHeader: 'ai:codex',
    });
    expect(res.status(), 'Worker actor must not drive any lifecycle edge').toBe(403);

    // CalmError → JSON body. `code: "forbidden"` is the wire
    // discriminator; `error` carries the validator's Display
    // (something like "wave lifecycle: actor Worker may not drive
    // …"). Pin both so a regression that silently drops the
    // discriminator or rewords the message in a way that loses
    // "Worker" trips this test.
    const body = (await res.json()) as { code?: string; error?: string };
    expect(body.code).toBe('forbidden');
    expect(body.error ?? '').toMatch(/Worker/i);

    // The wave stays in `draft` — validator runs before any write.
    expect(await getLifecycle(request, waveId)).toBe('draft');
  });
});
