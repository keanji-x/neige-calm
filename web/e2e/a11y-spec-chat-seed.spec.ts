// Spec-chat seed path — issue #682 PR-2, the #676 regression pin.
//
// #676 root cause: the FE gated every stop affordance on the overlay-
// derived `fsm === 'Working'`, but spec cards never publish a status
// overlay in production — so the Stop chip / ■ / Esc never showed even
// while a turn was visibly running. The fix gates on the harness phase,
// seeded from `GET /api/cards/{id}/spec/run` on mount. This suite pins
// the seed path at the browser level: the harness is forced into its
// phase BEFORE the page ever loads, so the only way the UI can know a
// turn is live is that seed read — exactly the "open the wave mid-turn"
// situation #676 shipped dead.
//
// The typing indicator (#657's regression, same overlay-gated class of
// bug) is asserted alongside the stop affordances: same `working` gate,
// same seed source.

import { test, expect } from '@playwright/test';

import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import { forceSpecPhase, getSpecCardId, getSpecRun } from './helpers/spec-chat';

test.describe('spec chat seed path (#676 pin)', () => {
  let waveId: string;
  let specCardId: string;

  test.beforeEach(async ({ request }) => {
    // Hermetic state — see `helpers/reset.ts`. The reset also shuts down
    // any harness a previous spec-chat test forced up.
    await resetReplayServer(request);
    const cove = await createUserCove(request, 'AtlasSpecSeed');
    const wave = await createWaveInCove(request, cove.id, 'Spec seed test');
    waveId = wave.id;
    specCardId = await getSpecCardId(request, waveId);
  });

  test('wave opened mid-turn renders all working affordances from the GET /spec/run seed', async ({
    page,
    request,
  }) => {
    // Force the harness into a running turn BEFORE any navigation. After
    // this the browser session starts cold: the only liveness source the
    // freshly-loaded page has is the `GET /spec/run` seed (plus the WS
    // replay of the same phase, which carries the identical wire value).
    const forced = await forceSpecPhase(request, specCardId, 'turn_running');
    expect(forced.new_phase).toBe('turn_running');
    // Cross-check the seed surface the FE is about to read.
    const run = await getSpecRun(request, specCardId);
    expect(run.phase).toBe('turn_running');

    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Spec seed test' }),
    ).toBeVisible();

    // The liveness UI lives in conversation mode (the header chip + stop
    // affordances render only there).
    await page.getByRole('button', { name: 'Conversation', exact: true }).click();
    await expect(page.getByLabel('Conversation', { exact: true })).toBeVisible();

    // #676 pin — the Stop chip in the conversation header.
    await expect(
      page.getByRole('button', { name: 'Stop spec turn' }),
    ).toBeVisible();
    // #676 pin — the ■ stop affordance in the input line.
    await expect(page.getByRole('button', { name: 'Stop turn' })).toBeVisible();
    // #657 pin — the typing indicator (same `working` gate).
    await expect(
      page.getByRole('status', { name: 'Spec Agent is working' }),
    ).toBeVisible();
    // The status chip reflects the seeded phase, styled as Working.
    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Turn Running');
    await expect(chip).toHaveAttribute('data-fsm', 'Working');
  });

  test('wave opened while idle renders no working affordances (inverse pin)', async ({
    page,
    request,
  }) => {
    await forceSpecPhase(request, specCardId, 'idle');

    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Spec seed test' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Conversation', exact: true }).click();
    await expect(page.getByLabel('Conversation', { exact: true })).toBeVisible();

    // The chip seeds Idle from the same read…
    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Idle');
    await expect(chip).toHaveAttribute('data-fsm', 'Idle');
    // …and none of the working affordances exist. Anchored after the chip
    // assertion so the "absence" checks run against a settled (seeded)
    // UI rather than a still-loading one.
    await expect(page.getByRole('button', { name: 'Stop spec turn' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Stop turn' })).toHaveCount(0);
    await expect(
      page.getByRole('status', { name: 'Spec Agent is working' }),
    ).toHaveCount(0);
  });
});
