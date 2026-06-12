// Spec-chat input path — issue #682 PR-2.
//
// `POST /api/cards/{id}/spec/input` works for real against the replay
// stub: `harness.observe(UserMessage)` is a pure MPSC enqueue, and the
// dev-forced harness is paused (it never issues codex RPCs), so a
// successful send is observable as: 200 response, textarea cleared, the
// queued echo entry in the transcript, no error surface, and NO phase
// churn afterwards (pinned via the trace ring buffer).

import { test, expect } from '@playwright/test';

import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import { forceSpecPhase, getSpecCardId } from './helpers/spec-chat';
import { clearEventTrace, getEventTrace } from './helpers/trace';

test.describe('spec chat input path', () => {
  let waveId: string;
  let specCardId: string;

  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
    const cove = await createUserCove(request, 'AtlasSpecInput');
    const wave = await createWaveInCove(request, cove.id, 'Spec input test');
    waveId = wave.id;
    specCardId = await getSpecCardId(request, waveId);
  });

  test('Enter sends the draft: 200, textarea clears, echo lands, phase stays put', async ({
    page,
    request,
  }) => {
    // A live (idle) harness must exist or the route answers the typed
    // `spec_harness_dormant` 409 — the dormant path is unit-tested; this
    // spec pins the happy path.
    await forceSpecPhase(request, specCardId, 'idle');

    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Spec input test' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Conversation', exact: true }).click();
    await expect(page.getByLabel('Conversation', { exact: true })).toBeVisible();

    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Idle');

    const textarea = page.getByRole('textbox', { name: 'Ask the Spec Agent' });
    await textarea.fill('Summarize the open risks');
    // Reset the trace AFTER setup settles so the no-churn assertion below
    // sees only what the send itself produced.
    await clearEventTrace(page);

    const [response] = await Promise.all([
      page.waitForResponse(
        (res) =>
          res.url().includes(`/api/cards/${specCardId}/spec/input`) &&
          res.request().method() === 'POST',
      ),
      textarea.press('Enter'),
    ]);
    expect(response.status()).toBe(200);

    // Pending resolves: the textarea clears and re-enables.
    await expect(textarea).toHaveValue('');
    await expect(textarea).toBeEnabled();
    // The FE-local echo entry lands in the transcript (queued until the
    // harness would emit the real item — which the paused dev harness
    // never does).
    await expect(page.getByText('Summarize the open risks')).toBeVisible();
    await expect(page.getByText('You · queued')).toBeVisible();
    // No error surface.
    await expect(page.getByRole('alert')).toHaveCount(0);

    // Phase stability: observe is enqueue-only, the paused harness never
    // starts a turn from it. Give a rogue transition a fair chance to
    // cross bus → WS → bridge, then assert the chip never moved and the
    // trace recorded zero phase churn.
    await page.waitForTimeout(500);
    await expect(chip).toHaveText('Idle');
    const phaseEvents = (await getEventTrace(page)).filter(
      (e) => e.ev === 'harness.phase.changed',
    );
    expect(phaseEvents, 'sending input must not churn the harness phase').toEqual([]);
  });
});
