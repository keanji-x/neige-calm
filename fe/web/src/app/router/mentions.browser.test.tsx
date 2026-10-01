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

function mount(path: string, storedText: string | null = null, pluginDescription = 'Develop issues and publish changes.') {
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path.startsWith('/api/areas/c1/mentions?q=bob')) return Promise.resolve(ok({ tags: [], tracks: [], blocks: [] }));
    if (request.path.startsWith('/api/areas/c1/mentions?')) return Promise.resolve(ok(CANDIDATES));
    if (request.method === 'POST' && request.path === '/api/tracks') return new Promise<ApiTransportResponse>(() => undefined);
    if (request.path === '/api/plugins') return Promise.resolve(ok([{
      id: 'dev.example', version: '1', enabled: false, state: 'disabled',
      manifest_name: 'development', manifest_description: pluginDescription,
      has_config: false, can_uninstall: false, can_disable: true,
    }]));
    if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
    if (request.path === '/api/areas/c1/tracks') return Promise.resolve(ok([TRACK]));
    if (request.path === '/api/tracks/w1') return Promise.resolve(ok({ track: TRACK, can_reopen: false, can_close: true, cards: [PLANNER_CARD], overlays: [] }));
    if (request.path === '/api/tracks/w1/conversations') return Promise.resolve(ok([ASSISTANT_ROW]));
    if (request.path.endsWith('/planner/run')) {
      return Promise.resolve(ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null }));
    }
    if (request.path.endsWith('/planner/input')) {
      storedText = (request.body as { text: string }).text;
      return Promise.resolve(ok({ card_id: PLANNER_CARD.id, worker_session_id: 'runtime', entry_id: 'entry-1' }));
    }
    if (request.path.includes('/harness/items?')) return Promise.resolve(ok(storedText === null ? [] : [{
      id: 1, worker_session_id: 'runtime', card_id: PLANNER_CARD.id, track_id: TRACK.id,
      thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: 'entry-1',
      item_type: 'userMessage', method: 'item/completed', params: '{}', created_at_ms: 50,
      input_segments: [{ presentation: 'user', text: storedText, attachments: [] }],
    }]));
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
  await expect.element(page.getByText('Deploy notes › b_1a2b', { exact: true })).toBeVisible();
  expect(document.querySelector('[data-nc-turn="you"]')?.textContent).toBe('see Deploy notes › b_1a2b');
  // A fresh route has no memory of the menu pick: the stored address still renders as a pill.
  cleanup();
  mount('/track/w1', sent.text);
  await openConversation(/Conversation Planner chat/);
  await expect.element(page.getByText('Deploy notes › b_1a2b', { exact: true })).toBeVisible();
  expect(document.querySelector('[data-nc-sent-mention]')?.getAttribute('title')).toBe(BLOCK_INSERT);
});

/** The group headings the open menu shows, top to bottom. */
async function groupOrder(): Promise<(string | null)[]> {
  return [...(await menu().findElement()).querySelectorAll('[role="group"]')].map((group) => group.getAttribute('aria-label'));
}

it.each([
  ['#', 'Tags', '#部署', CANDIDATES.tags[0].insert],
  ['/', 'Tracks', 'Deploy notes', CANDIDATES.tracks[0].insert],
  ['>', 'Blocks', 'Deploy notes › Rollback', BLOCK_INSERT],
])('narrows @%s to %s, searches without the prefix, and sends the pick\'s insert', async (prefix, group, chip, insert) => {
  await page.viewport(1440, 900);
  const { requests, mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Planner chat/);

  await userEvent.keyboard(`see @${prefix}`);
  await expect.element(menu().getByRole('group', { name: group }).getByRole('option').first()).toBeVisible();
  expect(await groupOrder()).toEqual([group]);
  expect(mentionReads()).toEqual(['/api/areas/c1/mentions?q=&track=w1']);
  await expect.element(page.getByRole('listbox', { name: 'Commands' })).not.toBeInTheDocument();

  await userEvent.keyboard('dep');
  await expect.poll(() => mentionReads().at(-1)).toBe('/api/areas/c1/mentions?q=dep&track=w1');
  await expect.element(menu().getByRole('group', { name: group }).getByRole('option').first()).toBeVisible();
  expect(await groupOrder()).toEqual([group]);
  await expect.element(page.getByRole('listbox', { name: 'Commands' })).not.toBeInTheDocument();

  await userEvent.keyboard('{Enter}');
  await expect.element(menu()).not.toBeInTheDocument();
  await expect.element(page.getByText(chip, { exact: true })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.filter((request) => request.path.endsWith('/planner/input')).length).toBe(1);
  const sent = requests.find((request) => request.path.endsWith('/planner/input'))!.body as { text: string };
  expect(sent.text).toBe(`see ${insert}`);
});

it('keeps the / command in the Planner conversation', async () => {
  mount('/track/w1');
  await openConversation(/Conversation Planner chat/);
  await userEvent.keyboard('/');
  await expect.element(page.getByRole('listbox', { name: 'Commands' }).getByRole('option', { name: /^new/ })).toBeVisible();
});

it.each([
  ['@', 'ask @bob', 'No matches'],
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

/** axe aria-allowed-attr: a combobox may not carry `aria-multiline` (#1891 drops the workaround). */
async function expectComboboxWithoutMultiline(name: string) {
  const field = await page.getByRole('combobox', { name }).findElement();
  await expect.poll(() => field.hasAttribute('aria-multiline')).toBe(false);
}

it('keeps aria-multiline off both trigger-bearing fields through mount, typing and a send', async () => {
  await page.viewport(1440, 900);
  const { requests } = mount('/track/w1');
  await openConversation(/Conversation Planner chat/);
  await expectComboboxWithoutMultiline('Message');
  await userEvent.keyboard('ask @dep');
  await expect.element(menu().getByRole('option').first()).toBeVisible();
  await expectComboboxWithoutMultiline('Message');
  await userEvent.keyboard('{Escape}{Enter}');
  await expect.poll(() => requests.filter((request) => request.path.endsWith('/planner/input')).length).toBe(1);
  await expectComboboxWithoutMultiline('Message');
  cleanup();

  mount('/area/c1/new');
  const sentence = page.getByRole('combobox', { name: 'What this track should do' });
  await expectComboboxWithoutMultiline('What this track should do');
  await sentence.click();
  await userEvent.keyboard('Continue @roll');
  await expect.element(menu().getByRole('option').first()).toBeVisible();
  await expectComboboxWithoutMultiline('What this track should do');
  await page.getByRole('button', { name: 'Create track' }).click();
  await expectComboboxWithoutMultiline('What this track should do');
});

it('offers disabled plugin documentation in an assistant conversation without area report access', async () => {
  const { requests, mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Side chat/);
  await userEvent.keyboard('@de');
  await expect.element(menu().getByRole('option', { name: /development/ })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toHaveTextContent('development');
  expect(mentionReads()).toEqual([]);
  expect(requests.some(request => request.method !== 'GET' && request.path.includes('/plugins'))).toBe(false);
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

it('includes plugin documentation in a new conversation first message without report reads', async () => {
  const { requests, mentionReads } = mount('/track/w1');
  await openConversation(/Conversation Side chat/);
  await userEvent.keyboard('/new');
  await expect.element(page.getByRole('listbox', { name: 'Commands' }).getByRole('option')).toBeVisible();
  await userEvent.keyboard('{Enter}');
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.click();
  await userEvent.keyboard('@development');
  await expect.element(menu().getByRole('option', { name: /development/ })).toBeVisible();
  await userEvent.keyboard('{Enter}');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.find(request => request.method === 'POST' && request.path === '/api/tracks/w1/conversations')).toBeDefined();
  const sent = requests.find(request => request.method === 'POST' && request.path === '/api/tracks/w1/conversations');
  expect(JSON.stringify(sent?.body)).toContain('Develop issues and publish changes.');
  expect(JSON.stringify(sent?.body)).toContain('documentation only');
  expect(mentionReads()).toEqual([]);
  expect(requests.some(request => request.method !== 'GET' && request.path.includes('/plugins'))).toBe(false);
});

it('shows the plugin name for a long guide and sends the complete description', async () => {
  await page.viewport(1440, 900);
  const description = 'Develop issues and publish changes. ' + 'A detailed guide for working on repository issues. '.repeat(6);
  const { requests } = mount('/track/w1', null, description);
  await openConversation(/Conversation Planner chat/);
  await userEvent.keyboard('@development');
  await expect.element(menu().getByRole('option', { name: /development/ })).toBeVisible();
  await menu().getByRole('option', { name: /development/ }).click();
  const field = page.getByRole('combobox', { name: 'Message' });
  await expect.poll(() => field.element().querySelector('[data-astryx-token]')?.textContent).toBe('development');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.find(request => request.method === 'POST' && request.path.endsWith('/planner/input'))).toBeDefined();
  expect(JSON.stringify(requests.find(request => request.method === 'POST' && request.path.endsWith('/planner/input'))?.body)).toContain(description.trim());
});
