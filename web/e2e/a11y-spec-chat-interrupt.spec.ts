// Spec-chat interrupt UI — issue #682 PR-2, the second half of the #676
// pin: a running turn must expose a Stop chip, a ■ input-line stop, and
// an Esc shortcut, and activating any of them must actually fire
// `POST /api/cards/{id}/spec/interrupt`.
//
// Probed stub behavior this suite pins (live replay binary, 2026-06):
// at `turn_running` the route answers 200 `{stopped: true}` — the
// `spec-harness-interrupt` operation enqueues fine; it is the codex
// `turn/interrupt` RPC behind it that the stub can never complete, so
// the harness parks at `issuing_interrupt` (no decay; a 30s watchdog
// would eventually wedge it — irrelevant here because each test acts,
// asserts, and ends well under that, and `beforeEach` resets drain the
// forced harness). Observable FE outcome of that wire reality:
//   * the chip moves to "Issuing Interrupt" (Working bucket);
//   * stop affordances stay visible but DISABLED (no double interrupt);
//   * the typing indicator closes (`working` is false at
//     `issuing_interrupt`);
//   * the FE-local "Turn stopped" system note lands (interrupted turns
//     never emit `item/completed`, so this row is the only feedback);
//   * no error surface.

import { test, expect, type Page } from '@playwright/test';

import { createUserCove, createWaveInCove, resetReplayServer } from './helpers/reset';
import { forceSpecPhase, getSpecCardId } from './helpers/spec-chat';

test.describe('spec chat interrupt UI', () => {
  let waveId: string;
  let specCardId: string;

  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
    const cove = await createUserCove(request, 'AtlasSpecStop');
    const wave = await createWaveInCove(request, cove.id, 'Spec interrupt test');
    waveId = wave.id;
    specCardId = await getSpecCardId(request, waveId);
  });

  /** Open the wave mid-turn and land in conversation mode. */
  async function openRunningConversation(page: Page): Promise<void> {
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Spec interrupt test' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Conversation', exact: true }).click();
    await expect(page.getByLabel('Conversation', { exact: true })).toBeVisible();
    await expect(page.locator('.report-convo-state')).toHaveText('Turn Running');
  }

  /** Shared post-interrupt assertions — see the header comment. */
  async function expectInterruptOutcome(page: Page): Promise<void> {
    const chip = page.locator('.report-convo-state');
    await expect(chip).toHaveText('Issuing Interrupt');
    await expect(chip).toHaveAttribute('data-fsm', 'Working');
    // Stop affordances visible but inert while the interrupt is in flight.
    await expect(page.getByRole('button', { name: 'Stop spec turn' })).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Stop turn' })).toBeDisabled();
    // Typing indicator closes (`working` excludes `issuing_interrupt`).
    await expect(
      page.getByRole('status', { name: 'Spec Agent is working' }),
    ).toHaveCount(0);
    // The FE-local system note is the user's only "it stopped" feedback.
    await expect(page.locator('.report-convo-system')).toContainText('Turn stopped');
    // And the 200 `{stopped: true}` answer means no error surface.
    await expect(page.getByRole('alert')).toHaveCount(0);
  }

  test('Stop chip click fires /spec/interrupt and the UI settles on the stopped state', async ({
    page,
    request,
  }) => {
    await forceSpecPhase(request, specCardId, 'turn_running');
    await openRunningConversation(page);

    const stopChip = page.getByRole('button', { name: 'Stop spec turn' });
    await expect(stopChip).toBeVisible();
    await expect(stopChip).toBeEnabled();
    // The ■ input-line twin is up too.
    await expect(page.getByRole('button', { name: 'Stop turn' })).toBeVisible();

    const [response] = await Promise.all([
      page.waitForResponse(
        (res) =>
          res.url().includes(`/api/cards/${specCardId}/spec/interrupt`) &&
          res.request().method() === 'POST',
      ),
      stopChip.click(),
    ]);
    expect(response.status()).toBe(200);
    const body = (await response.json()) as { stopped: boolean };
    expect(body.stopped).toBe(true);

    await expectInterruptOutcome(page);
  });

  test('Esc from the conversation input fires /spec/interrupt (keyboard parity)', async ({
    page,
    request,
  }) => {
    await forceSpecPhase(request, specCardId, 'turn_running');
    await openRunningConversation(page);

    // Entering conversation mode auto-focuses the input — the keyboard
    // user starts here without a single Tab. Esc is handled by the
    // document-level listener for any focus inside the conversation
    // region.
    const textarea = page.getByRole('textbox', { name: 'Ask the Spec Agent' });
    await expect(textarea).toBeFocused();

    const [response] = await Promise.all([
      page.waitForResponse(
        (res) =>
          res.url().includes(`/api/cards/${specCardId}/spec/interrupt`) &&
          res.request().method() === 'POST',
      ),
      page.keyboard.press('Escape'),
    ]);
    expect(response.status()).toBe(200);
    const body = (await response.json()) as { stopped: boolean };
    expect(body.stopped).toBe(true);

    await expectInterruptOutcome(page);
  });
});
