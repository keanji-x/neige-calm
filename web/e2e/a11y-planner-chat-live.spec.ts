// Planner-chat live path — issue #682 PR-2.
//
// Counterpart to `a11y-planner-chat-seed.spec.ts`: the page is open FIRST,
// then the harness phase is forced through a full turn lifecycle
// (`idle → issuing_turn → turn_running → turn_completed`). Every UI
// update below must therefore ride the `harness.phase.changed` WS event
// — no reload, no re-seed. The trace ring buffer pins the wire shape
// (snake_case `new_phase`, correct `card_id`) so a serde rename or a
// topic-scoping regression fails here rather than shipping a UI that
// silently never updates.

import { test, expect } from '@playwright/test';

import { createUserArea, createTrackInArea, resetReplayServer } from './helpers/reset';
import { forcePlannerPhase, getPlannerCardId } from './helpers/planner-chat';
import { getEventTrace } from './helpers/trace';

test.describe('planner chat live phase updates', () => {
  let trackId: string;
  let plannerCardId: string;

  test.beforeEach(async ({ request }) => {
    await resetReplayServer(request);
    const area = await createUserArea(request, 'AtlasPlannerLive');
    const track = await createTrackInArea(request, area.id, 'Planner live test');
    trackId = track.id;
    plannerCardId = await getPlannerCardId(request, trackId);
  });

  test('phase forces update the conversation UI live and pin the WS wire shape', async ({
    page,
    request,
  }) => {
    // Navigate BEFORE any force: the harness does not even exist yet
    // (`GET /planner/run` answers `{phase: null}`), so the chip sits on the
    // overlay-derived 'Starting' fallback. Everything after this point
    // arrives over the live WS stream.
    await page.goto(`/calm/track/${trackId}?trace=1`);
    await expect(
      page.getByRole('heading', { level: 1, name: 'Planner live test' }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Open conversation' }).click();
    await expect(page.getByRole('complementary', { name: 'Conversation drawer' }))
      .toHaveClass(/report-conversation-drawer--open/);

    const chip = page.locator('.report-convo-state');
    const stopChip = page.getByRole('button', { name: 'Stop planner turn' });
    const typing = page.getByRole('status', { name: 'Planner Agent is working' });
    await expect(chip).toHaveText('Starting');

    // idle — the first force stands the dev harness up.
    await forcePlannerPhase(request, plannerCardId, 'idle');
    await expect(chip).toHaveText('Idle');
    await expect(chip).toHaveAttribute('data-fsm', 'Idle');
    await expect(stopChip).toHaveCount(0);
    await expect(typing).toHaveCount(0);

    // issuing_turn — Working bucket; stop affordances + typing indicator
    // open up while the turn/start RPC would be in flight.
    await forcePlannerPhase(request, plannerCardId, 'issuing_turn');
    await expect(chip).toHaveText('Issuing Turn');
    await expect(chip).toHaveAttribute('data-fsm', 'Working');
    await expect(stopChip).toBeVisible();
    await expect(typing).toBeVisible();

    // turn_running — still Working, affordances stay.
    await forcePlannerPhase(request, plannerCardId, 'turn_running');
    await expect(chip).toHaveText('Turn Running');
    await expect(chip).toHaveAttribute('data-fsm', 'Working');
    await expect(stopChip).toBeVisible();
    await expect(typing).toBeVisible();

    // turn_completed — back to the Idle bucket; every working affordance
    // closes again, live, without a reload.
    await forcePlannerPhase(request, plannerCardId, 'turn_completed');
    await expect(chip).toHaveText('Turn Completed');
    await expect(chip).toHaveAttribute('data-fsm', 'Idle');
    await expect(stopChip).toHaveCount(0);
    await expect(typing).toHaveCount(0);

    // Wire-shape pin: the chip assertions above prove the UI consumed the
    // events; now prove WHAT it consumed. `harness.phase.changed` payloads
    // are snake_case (`new_phase`, `card_id`, …) — a camelCase drift would
    // leave `ev.data.new_phase` undefined and the UI frozen, which is
    // exactly the "tests green, feature dead" failure mode this suite
    // exists to catch.
    const phaseEvents = (await getEventTrace(page)).filter(
      (e) => e.ev === 'harness.phase.changed',
    );
    expect(phaseEvents.length).toBeGreaterThanOrEqual(4);
    for (const evt of phaseEvents) {
      const data = evt.data as Record<string, unknown>;
      expect(data['card_id'], 'snake_case card_id').toBe(plannerCardId);
      expect(typeof data['new_phase'], 'snake_case new_phase').toBe('string');
      expect(typeof data['runtime_id'], 'snake_case runtime_id').toBe('string');
      expect(data).not.toHaveProperty('newPhase');
      expect(data).not.toHaveProperty('cardId');
    }
    // The forced sequence arrived in order. Filtered to the four forced
    // tags so an (allowed) initial `pending_thread_start` emit from the
    // harness stand-up can't flake the ordering assertion.
    const forcedTags = ['idle', 'issuing_turn', 'turn_running', 'turn_completed'];
    const seen = phaseEvents
      .map((e) => (e.data as { new_phase?: string }).new_phase)
      .filter((p): p is string => p !== undefined && forcedTags.includes(p));
    expect(seen).toEqual(forcedTags);
  });
});
