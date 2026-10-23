import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { renderPlannerSwitchFixture } from './planner-switch-fixture.tsx';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });
const posts = (fixture: ReturnType<typeof renderPlannerSwitchFixture>) => fixture.requests.filter(request => request.method === 'POST');
async function open() {
  await page.getByRole('button', { name: /Conversation Other conversation/ }).click();
  await expect.element(page.getByText('Ready for switching')).toBeVisible();
  return page.getByRole('combobox', { name: 'Message' });
}

it.each([1280, 390])('promotes through the real menu and Enter flow at width %s', async width => {
  await page.viewport(width, 844);
  const fixture = renderPlannerSwitchFixture(); const input = await open();
  await input.fill('/planner');
  await expect.element(page.getByRole('option', { name: /^planner / })).toBeVisible();
  await userEvent.keyboard('{Enter}'); expect(posts(fixture)).toHaveLength(0);
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => posts(fixture).map(request => request.body)).toEqual([{
    target_assistant_card_id: 'second-planner', expected_planner_card_id: 'daily-planner', expected_revision: 7,
  }]);
  if (width === 1280) await expect.element(page.getByRole('button', { name: /Role: Member/ })).toBeVisible();
  await expect.poll(() => fixture.client.getQueryData<{ planner_revision: number }>(['track', 'daily'])?.planner_revision).toBe(8);
  await expect.element(page.getByText('Ready for switching')).toBeVisible();
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await expect.element(page.getByRole('button', { name: /Conversation Other conversation/ })).toBeVisible();
  await expect.element(page.getByRole('button', { name: /Conversation Daily Planner conversation/ })).toBeVisible();
  expect(posts(fixture)).toHaveLength(1);
});

it('keeps the draft and shows a specific refusal', async () => {
  await page.viewport(1280, 800);
  const fixture = renderPlannerSwitchFixture({ switch: () => ({ status: 409, statusText: 'Conflict',
    body: { code: 'planner_open_ask', error: 'open question', card_id: 'second-planner' } }) });
  const input = await open(); await input.fill('/planner');
  await page.getByRole('option', { name: /^planner / }).click();
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('alert')).toHaveTextContent('Resolve the open questions first. (Target Assistant: Other conversation)');
  await expect.element(input).toHaveTextContent('/planner'); expect(posts(fixture)).toHaveLength(1);
});

it('rejects arguments without a write', async () => {
  await page.viewport(1280, 800);
  const fixture = renderPlannerSwitchFixture(); const input = await open(); await input.fill('/planner now');
  await page.getByRole('button', { name: 'Send' }).click();
  await expect.element(page.getByRole('alert')).toHaveTextContent('Use /planner without arguments.');
  await expect.element(input).toHaveTextContent('/planner now'); expect(posts(fixture)).toHaveLength(0);
});

it('blocks another submission while the first request is pending', async () => {
  await page.viewport(1280, 800);
  let finish!: () => void;
  const fixture = renderPlannerSwitchFixture({ switch: (_request, commit) => new Promise(resolve => {
    finish = () => { commit(); resolve(fixture.receipt()); };
  }) });
  const input = await open(); await input.fill('/planner');
  await page.getByRole('option', { name: /^planner / }).click(); await userEvent.keyboard('{Enter}');
  await expect.poll(() => posts(fixture).length).toBe(1);
  await input.fill('/planner'); await page.getByRole('option', { name: /^planner / }).click(); await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('alert')).toHaveTextContent('was not accepted');
  expect(posts(fixture)).toHaveLength(1); finish();
  await expect.element(page.getByRole('button', { name: /Role: Member/ })).toBeVisible();
  expect(posts(fixture)).toHaveLength(1);
});
