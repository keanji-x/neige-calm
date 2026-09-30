// `@` on the real routes, in a real engine (#1881 S2): the Planner's conversation and the
// new-track sentence offer the area's tags, reports and blocks from the mentions endpoint; a
// track's assistant conversation, which cannot read `area/reports/`, offers nothing.
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { MentionCandidates } from '../../../../core/api/generated/wire.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const TRACK = { id: 'w1', area_id: 'c1', title: 'Test track', sort: 1, cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
const PLANNER_CARD = { id: 'card-planner', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1, payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
const ASSISTANT_ROW = { id: 'conv-assistant-1', trackId: 'w1', title: 'Side chat', kind: 'track-assistant', state: 'idle', updatedAt: 30, lastTurnCompletedAt: null };
const BLOCK_INSERT = '@`area/reports/Deploy notes.md#b_1a2b`';
const CANDIDATES: MentionCandidates = {
  tags: [{ label: '部署', track_count: 2, insert: '@`tag:部署`' }],
  tracks: [{ label: 'Deploy notes', track_id: 'w9', insert: '@`area/reports/Deploy notes.md`' }],
  blocks: [{ label: 'Rollback', block_id: 'b_1a2b', track_title: 'Deploy notes', track_id: 'w9', insert: BLOCK_INSERT }],
};

function mount(path: string) {
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path.startsWith('/api/areas/c1/mentions?q=bob')) return Promise.resolve(ok({ tags: [], tracks: [], blocks: [] }));
    if (request.path.startsWith('/api/areas/c1/mentions?')) return Promise.resolve(ok(CANDIDATES));
    if (request.method === 'POST' && request.path === '/api/tracks') return new Promise<ApiTransportResponse>(() => undefined);
    if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
    if (request.path === '/api/tracks/w1') return Promise.resolve(ok({ track: TRACK, can_reopen: false, cards: [PLANNER_CARD], overlays: [] }));
    if (request.path === '/api/tracks/w1/conversations') return Promise.resolve(ok([ASSISTANT_ROW]));
    if (request.path.endsWith('/planner/run')) {
      return Promise.resolve(ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null }));
    }
    if (request.path.endsWith('/planner/input')) return Promise.resolve(ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', entry_id: null }));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(), unauthorized, onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  const mentionReads = () => requests.filter((request) => request.path.startsWith('/api/areas/c1/mentions?')).map((request) => request.path);
  return { requests, mentionReads };
}

async function openConversation(name: RegExp) {
  await page.getByRole('button', { name }).click();
  const field = page.getByRole('combobox', { name: 'Message' });
  await expect.element(field).toBeVisible();
  await field.click();
}

const menu = () => page.getByRole('listbox', { name: 'Mention' });

it('groups the area\'s tags, reports and blocks under @ in the Planner conversation, and sends the pick', async () => {
  await page.viewport(1440, 900);
  const { requests, mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Planner chat/);

  await userEvent.keyboard('see @dep');
  await expect.element(menu().getByRole('group', { name: 'Blocks' }).getByRole('option')).toBeVisible();
  expect(await groupOrder()).toEqual(['Tags', 'Tracks', 'Blocks']);
  expect(mentionReads().at(-1)).toBe('/api/areas/c1/mentions?q=dep&track=w1');

  await userEvent.keyboard('{ArrowDown}{ArrowDown}{Enter}');
  await expect.element(menu()).not.toBeInTheDocument();
  await expect.element(page.getByText('Deploy notes › Rollback')).toBeVisible();
  await userEvent.keyboard('{Enter}');

  await expect.poll(() => requests.filter((request) => request.path.endsWith('/planner/input')).length).toBe(1);
  const sent = requests.find((request) => request.path.endsWith('/planner/input'))!.body as { text: string };
  expect(sent.text).toBe(`see ${BLOCK_INSERT}`);
});

/** The group headings the open menu shows, top to bottom. */
async function groupOrder(): Promise<(string | null)[]> {
  return [...(await menu().findElement()).querySelectorAll('[role="group"]')].map((group) => group.getAttribute('aria-label'));
}

it('asks for recommendations on a bare @, blocks first', async () => {
  const { mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Planner chat/);
  await userEvent.keyboard('@');
  await expect.element(menu().getByRole('option', { name: /部署/ })).toBeVisible();
  expect(mentionReads()).toEqual(['/api/areas/c1/mentions?q=&track=w1']);
  expect(await groupOrder()).toEqual(['Blocks', 'Tracks', 'Tags']);
  /* The first row is the one Enter takes. */
  expect(menu().getByRole('option', { selected: true }).element().textContent).toBe('RollbackDeploy notes');
});

it('keeps the / command in the Planner conversation', async () => {
  mount('/track/w1');
  await openConversation(/Conversation Planner chat/);
  await userEvent.keyboard('/');
  await expect.element(page.getByRole('listbox', { name: 'Commands' }).getByRole('option', { name: /^new/ })).toBeVisible();
});

it.each([
  ['@', 'ask @bob', 'Nothing in this area matches'],
  ['/', 'check /tmp/x', 'No command by that name'],
])('sends over an empty %s menu once and keeps the Planner drawer open', async (_, text, empty) => {
  await page.viewport(1440, 900);
  const { requests } = mount('/track/w1');
  await openConversation(/Conversation Planner chat/);
  await userEvent.keyboard(text);
  await expect.element(page.getByText(empty)).toBeVisible();
  await userEvent.keyboard('{Enter}');
  const sends = () => requests.filter((request) => request.path.endsWith('/planner/input'))
    .map((request) => (request.body as { text: string }).text);
  await expect.poll(sends).toEqual([text]);
  /* A frame for a close the send might have set off. */
  await new Promise((resolve) => { requestAnimationFrame(resolve); });
  expect(document.querySelector('[data-nc-escape-layer]')).not.toBeNull();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toBeInTheDocument();
  expect(sends()).toEqual([text]);
});

it('offers no @ menu in a track\'s assistant conversation', async () => {
  const { mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Side chat/);
  await userEvent.keyboard('@de');
  /* Longer than the source's own delay: a menu that was coming would have asked by now. */
  await new Promise((resolve) => { setTimeout(resolve, 400); });
  /* Astryx keeps an empty, closed listbox in the tree; what matters is that nothing opened. */
  expect(document.querySelector('[role="option"]')).toBeNull();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toHaveAttribute('aria-expanded', 'false');
  expect(mentionReads()).toEqual([]);
});

it('offers @ in the new-track sentence, lets Enter pick, and creates with the insert in the first message', async () => {
  await page.viewport(1440, 900);
  const { requests, mentionReads } = mount('/area/c1/new');
  const field = page.getByRole('combobox', { name: 'What this track should do' });
  await field.click();
  await userEvent.keyboard('Continue @roll');
  await expect.element(menu().getByRole('option', { name: /Rollback/ })).toBeVisible();
  expect(mentionReads().at(-1)).toBe('/api/areas/c1/mentions?q=roll');

  /* Attached under the composer at its full width, clear of the greeting above it. */
  const popover = (await menu().findElement()).closest('[popover]')!.getBoundingClientRect();
  const composer = document.querySelector('[data-nc-new-track-message]')!.getBoundingClientRect();
  const greeting = (await page.getByRole('heading', { level: 1 }).findElement()).getBoundingClientRect();
  expect(Math.round(popover.left)).toBe(Math.round(composer.left));
  expect(Math.round(popover.right)).toBe(Math.round(composer.right));
  expect(popover.top).toBeGreaterThanOrEqual(composer.bottom - 1);
  expect(popover.top).toBeLessThanOrEqual(composer.bottom + 12);
  expect(popover.top).toBeGreaterThan(greeting.bottom);

  await userEvent.keyboard('{ArrowUp}{Enter}');
  await expect.element(menu()).not.toBeInTheDocument();
  expect(requests.some((request) => request.method === 'POST' && request.path === '/api/tracks')).toBe(false);

  await page.getByRole('button', { name: 'Create track' }).click();
  await expect.poll(() => requests.filter((request) => request.method === 'POST' && request.path === '/api/tracks').length).toBe(1);
  const created = requests.find((request) => request.method === 'POST' && request.path === '/api/tracks')!.body as { first_message: string };
  expect(created.first_message).toBe(`Continue ${BLOCK_INSERT}\u00A0`);
});
