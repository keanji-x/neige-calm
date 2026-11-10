import '../../styles/entry.css';
import { act, cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { useRememberedConversations } from '../conversations/public.tsx';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { renderDailyFixture, renderWorkingDailySideFixture } from './daily-planner-fixture.tsx';
import { historicalConversationRun, conversationCardId } from './conversation-read-data.ts';
import type { TrackDetailWire } from '../../../../core/domain/track.ts';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { queryKeys } from '../providers/queries.ts';
import { renderConversationReadFixture } from './conversation-read-fixture.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 800); });

it.each([[390, false], [1280, false], [390, true], [1280, true]] as const)('reads, fails and confirms recovery in the production drawer (%ipx, initial failure: %s)', async (width, initialFailure) => {
  await page.viewport(width, 844);
  const fixture = renderConversationReadFixture(initialFailure);
  await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  await expect.element(page.getByText('Retained reply.', { exact: true })).toBeVisible();
  if (!initialFailure) await expect.element(page.getByText('Running', { exact: true })).toBeVisible();
  const field = page.getByRole('combobox', { name: 'Message' });
  await userEvent.type(field, 'Keep this draft.');
  if (!initialFailure) {
    fixture.failRun(true);
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun('daily-planner') }); });
  }
  await expect.element(page.getByText('Status unconfirmed', { exact: true })).toBeVisible();
  expect(screen.queryByText('Running', { exact: true })).toBeNull();
  const retry = page.getByRole('button', { name: 'Reload status' });
  await expect.element(retry).toBeVisible();
  const before = fixture.requests.length;
  fixture.failRun(false);
  const release = fixture.pauseRun();
  try {
    await retry.click();
    if (initialFailure) {
      await expect.element(retry).not.toBeInTheDocument();
      await page.getByRole('button', { name: 'Status unconfirmed', expanded: false }).click();
      await expect.element(page.getByText('Checking the conversation’s current state.')).toBeVisible();
    } else {
      await expect.element(retry).toHaveAttribute('aria-busy', 'true');
      await retry.click({ force: true });
    }
    expect(fixture.requests.slice(before).filter(request => request.path.includes('/planner/run?contract=conversation_v1'))).toHaveLength(1);
    expect(screen.queryByText('Running', { exact: true })).toBeNull();
    await expect.element(page.getByText('Status unconfirmed', { exact: true })).toBeVisible();
    await expect.element(field).toHaveTextContent('Keep this draft.');
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
    await page.screenshot({ path: `./__screenshots__/conversation-status-unconfirmed-${width}-${initialFailure}.png` });
  } finally { release(); }
  await expect.element(page.getByText('Running', { exact: true })).toBeVisible();
  expect(screen.queryByText('Status unconfirmed', { exact: true })).toBeNull();
  await expect.element(field).toHaveTextContent('Keep this draft.');
  await expect.element(page.getByText('Retained reply.', { exact: true })).toBeVisible();
  expect(fixture.requests.slice(before).every(request => request.method === 'GET')).toBe(true);
});


function SummaryProbe({ observe }: { observe: (rows: readonly Conversation[]) => void }) {
  observe(useRememberedConversations());
  return null;
}

it.each([
  ['/track/daily', 'empty'],
  ['/track/daily', 'closed'],
  ['/track/daily', 'draft'],
  ['/?day=2026-10-04', 'empty'],
  ['/?day=2026-10-04', 'closed'],
  ['/?day=2026-10-04', 'draft'],
] as const)('protects the real compact dock summary from hidden sibling lists (%s, %s)', async (initial, selection) => {
  await page.viewport(390, 844);
  let summaries: readonly Conversation[] = [];
  let wedged = false;
  let title = 'Daily Planner conversation';
  const preferences = createUiPreferences();
  const fixture = renderDailyFixture({ initial, uiPreferences: preferences,
    routeProbe: <SummaryProbe observe={rows => { summaries = rows; }} />,
    withTransport: transport => ({ async send(request) {
      const response = await transport.send(request);
      if (request.path !== '/api/tracks/daily') return response;
      const detail = response.body as TrackDetailWire;
      return { ...response, body: { ...detail, cards: detail.cards.map(card => card.id !== 'daily-planner' ? card : {
        ...card, title, runtime: { worker_session_id: 'runtime', kind: 'codex', status: 'idle' },
      }) } };
    } }),
    reply: request => {
      if (request.path.includes('/planner/run?contract=conversation_v1')) return { status: 200, statusText: 'OK', body: historicalConversationRun({
        card_id: conversationCardId(request.path), worker_session_id: 'runtime', phase: wedged ? 'wedged' : 'idle', model: null,
        reasoning_effort: null, permission_mode: 'never', blocked_reason: null, pending_queue: [], manual_compact_supported: true,
        recovery: wedged ? { cause: 'wedged', action: 'restart_session' } : null, compact_outcome: null,
        context_usage_invalidated: false, running_turn: null, final_reply: null,
      }, 'idle', true) };
      return undefined;
    },
  });
  const summary = () => summaries.find(row => row.id === 'daily-planner');
  try {
    await expect.element(page.getByRole('textbox', { name: 'Chat message', exact: true })).toBeVisible();
    expect(preferences.conversation('daily')).toBeNull();
    expect(document.querySelector('dialog[open]')).toBeNull();
    if (selection === 'closed') {
      await page.getByRole('textbox', { name: 'Chat message', exact: true }).click();
      await expect.element(page.getByRole('dialog', { name: title, exact: true })).toBeVisible();
      await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
      await expect.poll(() => document.querySelector('dialog[open]')).toBeNull();
    } else if (selection === 'draft') {
      await page.viewport(1280, 844);
      await page.getByRole('button', { name: 'New conversation', exact: true }).click();
      await expect.element(page.getByRole('combobox', { name: 'Message', exact: true })).toBeVisible();
      await page.viewport(390, 844);
      await expect.element(page.getByRole('dialog', { name: 'Untitled', exact: true })).toBeVisible();
    }
    await expect.poll(() => fixture.requests.some(request => request.path.includes('/planner/run?contract=conversation_v1'))).toBe(true);
    wedged = true;
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.plannerRun('daily-planner') }); });
    await expect.poll(() => summary()?.state).toBe('failed');
    const settled = summaries;
    // Repeated stale list publications must neither win nor cause summary oscillation.
    for (let refresh = 0; refresh < 2; refresh += 1) {
      await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackConversations('daily') }); });
      expect(summary()?.state).toBe('failed');
      expect(summaries).toBe(settled);
    }
    // Once the actual reader leaves, the ordinary list is authoritative again.
    if (selection === 'draft') await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
    await page.viewport(1280, 844);
    title = 'Refreshed after reader departure';
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('daily') }); });
    await expect.poll(() => ({ title: summary()?.title, state: summary()?.state })).toEqual({ title, state: 'idle' });
  } finally { cleanup(); fixture.client.clear(); }
});

it('keeps each active pane run authoritative and releases a closed side to server rows', async () => {
  await page.viewport(1440, 900);
  let summaries: readonly Conversation[] = [];
  const fixture = renderWorkingDailySideFixture({ initial: '/track/daily?panel=conversations',
    routeProbe: <SummaryProbe observe={rows => { summaries = rows; }} />,
    withTransport: transport => ({ async send(request) {
      const response = await transport.send(request);
      if (request.method !== 'GET' || request.path !== '/api/tracks/daily/conversations') return response;
      return { ...response, body: (response.body as readonly Conversation[]).map(row => ({ ...row, state: 'idle' })) };
    } }),
  });
  const state = (id: string) => summaries.find(row => row.id === id)?.state;
  try {
    await page.getByRole('button', { name: 'Conversation Daily Planner conversation', exact: true }).click();
    const main = page.getByRole('complementary', { name: 'Daily Planner conversation', exact: true });
    await expect.element(main.getByText('Working on', { exact: false })).toBeVisible();
    await main.getByRole('combobox', { name: 'Message', exact: true }).fill('/side Explore independently');
    await userEvent.keyboard('{Enter}');
    const side = page.getByRole('region', { name: 'Side conversation · Codex', exact: true });
    await expect.element(side.getByRole('button', { name: 'Stop', exact: true })).toBeVisible();
    const child = fixture.rows[0].id;
    await expect.poll(() => [state('daily-planner'), state(child)]).toEqual(['running', 'turn_pending']);
    await page.getByRole('button', { name: 'Terminal', exact: true }).click();
    await expect.element(page.getByRole('button', { name: 'Hide terminal', exact: true })).toBeVisible();
    for (const width of [1440, 390, 1440]) {
      await page.viewport(width, 900);
      await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackConversations('daily') }); });
      expect([state('daily-planner'), state(child)]).toEqual(['running', 'turn_pending']);
    }
    await page.getByRole('button', { name: 'Discussion', exact: true }).click();
    await side.getByRole('button', { name: 'Close side conversation', exact: true }).click();
    await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackConversations('daily') }); });
    await expect.poll(() => [state('daily-planner'), state(child)]).toEqual(['running', 'idle']);
  } finally { cleanup(); fixture.client.clear(); }
});
