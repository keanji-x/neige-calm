// Planner-chat seed path — issue #682 PR-2, the #676 regression pin.
//
// #676 root cause: the FE gated every stop affordance on the overlay-
// derived `fsm === 'Working'`, but planner cards never publish a status
// overlay in production — so the Stop chip / ■ / Esc never showed even
// while a turn was visibly running. The fix gates on the harness phase,
// seeded from `GET /api/cards/{id}/planner/run` on mount. This suite pins
// the seed path at the browser level: the harness is forced into its
// phase BEFORE the page ever loads, so the only way the UI can know a
// turn is live is that seed read — exactly the "open the track mid-turn"
// situation #676 shipped dead.
//
// The typing indicator (#657's regression, same overlay-gated class of
// bug) is asserted alongside the stop affordances: same `working` gate,
// same seed source.

import { test, expect } from '@playwright/test';

import { createUserArea, createTrackInArea, resetReplayServer } from './helpers/reset';
import {
  dropServerPhaseFrames,
  forcePlannerPhase,
  getPlannerCardId,
  getPlannerRun,
} from './helpers/planner-chat';

test.describe('planner chat seed path (#676 pin)', () => {
  let trackId: string;
  let plannerCardId: string;

  test.beforeEach(async ({ request }) => {
    // Hermetic state — see `helpers/reset.ts`. The reset also shuts down
    // any harness a previous planner-chat test forced up.
    await resetReplayServer(request);
    const area = await createUserArea(request, 'AtlasPlannerSeed');
    const track = await createTrackInArea(request, area.id, 'Planner seed test');
    trackId = track.id;
    plannerCardId = await getPlannerCardId(request, trackId);
  });

  test('track opened mid-turn renders all working affordances from the GET /planner/run seed', async ({
    page,
    request,
  }) => {
    // Force the harness into a running turn BEFORE any navigation. After
    // this the browser session starts cold. The WS stream would normally
    // replay the forced `harness.phase.changed` too (it subscribes with
    // `since: 0`), so the frame-drop filter below removes that second
    // source: the only way the freshly-loaded page can learn the turn is
    // live is the `GET /planner/run` seed.
    const forced = await forcePlannerPhase(request, plannerCardId, 'turn_running');
    expect(forced.new_phase).toBe('turn_running');
    // Cross-check the seed surface the FE is about to read.
    const run = await getPlannerRun(request, plannerCardId);
    expect(run.phase).toBe('turn_running');

    await dropServerPhaseFrames(page);
    await page.goto(`/calm/track/${trackId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Planner seed test' }),
    ).toBeVisible();

    // The liveness UI lives in conversation mode (the header chip + stop
    // affordances render only there).
    await page.getByRole('button', { name: 'Open conversation' }).click();
    await expect(page.getByRole('complementary', { name: 'Conversation drawer' }))
      .toHaveClass(/report-conversation-drawer--open/);

    // #676 pin — the Stop chip in the conversation header.
    await expect(
      page.getByRole('button', { name: 'Stop planner turn' }),
    ).toBeVisible();
    // #676 pin — the ■ stop affordance in the input line.
    await expect(page.getByRole('button', { name: 'Stop turn' })).toBeVisible();
    // #657 pin — the typing indicator (same `working` gate).
    await expect(
      page.getByRole('status', { name: 'Planner Agent is working' }),
    ).toBeVisible();
    // The status chip reflects the seeded phase, styled as Working.
    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Turn Running');
    await expect(chip).toHaveAttribute('data-fsm', 'Working');
  });

  test('track opened while idle renders no working affordances (inverse pin)', async ({
    page,
    request,
  }) => {
    await forcePlannerPhase(request, plannerCardId, 'idle');

    // Same hardening as the mid-turn test: with the WS phase replay
    // dropped, the Idle chip below can only come from the REST seed.
    await dropServerPhaseFrames(page);
    await page.goto(`/calm/track/${trackId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Planner seed test' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Open conversation' }).click();
    await expect(page.getByRole('complementary', { name: 'Conversation drawer' }))
      .toHaveClass(/report-conversation-drawer--open/);

    // The chip seeds Idle from the same read…
    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Idle');
    await expect(chip).toHaveAttribute('data-fsm', 'Idle');
    // …and none of the working affordances exist. Anchored after the chip
    // assertion so the "absence" checks run against a settled (seeded)
    // UI rather than a still-loading one.
    await expect(page.getByRole('button', { name: 'Stop planner turn' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Stop turn' })).toHaveCount(0);
    await expect(
      page.getByRole('status', { name: 'Planner Agent is working' }),
    ).toHaveCount(0);
  });
});
