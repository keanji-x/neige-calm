import '../../styles/entry.css';
// #2348 A4 in a real browser: a Planner conversation's approval setting beside the model picker, and a
// paused turn's request in the Questions drawer, through the production router.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => {
  cleanup(); document.getElementById('root')?.remove(); delete document.documentElement.dataset.theme;
  await page.viewport(1280, 720);
});

const TRACK = { id: 'approvals-track', area_id: 'area', title: 'Release 2.0', sort: 1, cwd: '/tmp',
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
const PLANNER = { id: 'planner', track_id: TRACK.id, kind: 'codex', title: 'Planner', sort: 1,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
const HOLD_TITLE = 'Run `cargo test -p calm-server` (cwd /work/neige)?';
const HOLD = {
  source: 'ask', key: 'ask:12', text: HOLD_TITLE, at_ms: 5, ask_id: 12,
  questions: [{ title: HOLD_TITLE, options: ['Allow', 'Allow for this session', 'Deny'] }],
  delivery: 'hold',
};

type Mode = 'never' | 'ask' | null;
type Options = Readonly<{
  theme?: 'light' | 'dark';
  mode?: Mode;
  asks?: readonly unknown[];
  /** The answer to `PUT /planner/permission-mode`; by default it stores the mode asked for. */
  putMode?: (body: { permission_mode: Mode }) => ApiTransportResponse;
  /** The answer to the hold ask's answer route. */
  answer?: () => ApiTransportResponse;
}>;

const answered = (status: number, body: unknown): ApiTransportResponse => ({
  status, statusText: status === 200 ? 'OK' : status === 204 ? 'No Content' : 'Error', body,
});

function setup({ theme = 'light', mode = 'never', asks = [], putMode, answer }: Options = {}) {
  const requests: ApiRequest[] = [];
  let stored: Mode = mode;
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [TRACK];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${TRACK.id}`) body = { track: TRACK, can_reopen: false, can_close: true, cards: [PLANNER],
      overlays: [{ id: 'activity', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity', updated_at: 5,
        payload: { schemaVersion: 4, working: false, attention: asks.length > 0 ? 'input' : 'none', activity_at_ms: 5, items: asks, cards: [] } }] };
    if (request.path.endsWith('/planner/run')) body = { card_id: PLANNER.id, worker_session_id: 'session',
      phase: asks.length > 0 ? 'turn_running' : 'idle', model: null, reasoning_effort: null, permission_mode: stored,
      blocked_reason: null, running_turn: asks.length > 0 ? { turn_id: 'turn-1', elapsed_ms: 42_000 } : null, attachments_supported: true };
    if (request.path.endsWith('/harness/live')) body = { turn_id: null, items: [] };
    if (request.path.includes('/harness/items')) body = [{ id: 1, worker_session_id: 'session', card_id: PLANNER.id,
      track_id: TRACK.id, thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage',
      method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text: 'The release checklist is green. Running the server tests next.' } }) }];
    if (request.method === 'PUT' && request.path === `/api/cards/${PLANNER.id}/planner/permission-mode`) {
      const sent = request.body as { permission_mode: Mode };
      const response = putMode?.(sent) ?? answered(200, { card_id: PLANNER.id, permission_mode: sent.permission_mode });
      if (response.status === 200) stored = sent.permission_mode;
      return response;
    }
    if (request.path === `/api/tracks/${TRACK.id}/asks/12/answer`) return answer?.() ?? answered(204, undefined);
    return { status: 200, statusText: 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: [`${APP_BASEPATH}/track/${TRACK.id}?panel=conversations`] }) });
  const container = document.createElement('div'); container.id = 'root'; document.body.append(container);
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => theme, setItem: () => {} }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>, { container });
  return { requests };
}

const modeWrites = (requests: readonly ApiRequest[]) => requests.filter((request) => request.method === 'PUT'
  && request.path.endsWith('/planner/permission-mode'));
const answers = (requests: readonly ApiRequest[]) => requests.filter((request) => request.path.endsWith('/answer'));

function expectNoHorizontalScroll() {
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
}

/** A phone keeps the composer's controls behind its options button; a desktop shows them in its footer. */
async function revealComposerControls() {
  const options = document.querySelector<HTMLElement>('summary[aria-label="对话选项"]');
  if (options === null) return;
  options.click();
  await expect.poll(() => options.closest('details')?.open).toBe(true);
}

it.each([
  ['light', 1280], ['dark', 1280], ['light', 390], ['dark', 390],
] as const)('reads the stored mode, and a pick is written and read back (%s, %ipx)', async (theme, width) => {
  await page.viewport(width, width < 600 ? 844 : 800);
  const { requests } = setup({ theme });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toBeVisible();
  if (width < 600) await revealComposerControls();
  const trigger = page.getByRole('button', { name: 'Approvals: Never' });
  await expect.element(trigger).toBeVisible();
  /* Beside the model picker, inside the composer, and the composer's Send still inside the viewport. */
  const triggerElement = await trigger.findElement();
  expect(triggerElement.closest('[data-nc-composer]')).not.toBeNull();
  const model = (await page.getByRole('button', { name: /^Model:/ }).findElement()).getBoundingClientRect();
  const box = triggerElement.getBoundingClientRect();
  expect(Math.abs(box.top - model.top)).toBeLessThan(4);
  expect(box.left).toBeGreaterThanOrEqual(model.right);
  expect(box.right).toBeLessThanOrEqual(window.innerWidth);
  expectNoHorizontalScroll();

  await trigger.click();
  const menu = page.getByRole('menu');
  await expect.element(menu).toBeVisible();
  const bounds = (await menu.findElement()).getBoundingClientRect();
  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(window.innerWidth);
  await expect.element(menu.getByRole('note')).toHaveTextContent('Applies from the next turn.');
  await menu.getByRole('menuitem', { name: /^Ask me/ }).click();

  await expect.poll(() => modeWrites(requests).length).toBe(1);
  expect(modeWrites(requests)[0]?.body).toEqual({ permission_mode: 'ask' });
  await expect.element(page.getByRole('button', { name: 'Approvals: Ask me' })).toBeVisible();
  expectNoHorizontalScroll();
});

it.each([
  ['a refusal', answered(403, { code: 'forbidden', error: 'Only you can change what the Planner may do.' }),
    'Only you can change what the Planner may do.'],
  ['an unknown outcome', answered(500, { code: 'internal', error: 'database is locked' }),
    'The approval setting change is unconfirmed.'],
] as const)('says so when a change fails with %s, and keeps showing the stored mode', async (_name, response, shown) => {
  await page.viewport(1280, 800);
  const { requests } = setup({ putMode: () => response });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await page.getByRole('button', { name: 'Approvals: Never' }).click();
  await page.getByRole('menu').getByRole('menuitem', { name: /^Ask me/ }).click();
  await expect.poll(() => modeWrites(requests).length).toBe(1);
  await expect.element(page.getByText(shown)).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Approvals: Never' })).toBeVisible();
});

it('shows no approval setting for a conversation that is not a Planner', async () => {
  await page.viewport(1280, 800);
  setup({ mode: null });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.element(page.getByRole('button', { name: /^Model:/ })).toBeVisible();
  expect(page.getByRole('button', { name: /^Approvals:/ }).query()).toBeNull();
});

it.each([
  ['light', 1280], ['dark', 1280], ['light', 390], ['dark', 390],
] as const)('puts a paused turn\'s request to the reader as options only (%s, %ipx)', async (theme, width) => {
  await page.viewport(width, width < 600 ? 844 : 800);
  const { requests } = setup({ theme, mode: 'ask', asks: [HOLD] });
  /* The Notifications row says what waits, and offers no Dismiss: only an answer ends it. */
  const notice = page.getByRole('region', { name: 'Notifications' });
  if (width >= 600) {
    await expect.element(notice.getByText('Waiting for your approval')).toBeVisible();
    expect(notice.getByRole('button', { name: /^Dismiss/ }).query()).toBeNull();
  }
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await expect.element(ask).toBeVisible();
  expect((await ask.findElement()).closest('[data-nc-composer]')).not.toBeNull();
  await expect.element(ask.getByText('Turn paused, waiting for your approval')).toBeVisible();
  await expect.element(ask.getByRole('heading', { name: HOLD_TITLE })).toBeVisible();
  expect(ask.getByRole('textbox').query()).toBeNull();
  expect(ask.getByRole('button', { name: 'Answer' }).query()).toBeNull();
  expect(ask.getByRole('button', { name: /^Dismiss/ }).query()).toBeNull();
  expectNoHorizontalScroll();
  const field = (await page.getByRole('combobox', { name: 'Message' }).findElement()).getBoundingClientRect();
  expect(field.bottom).toBeLessThanOrEqual(window.innerHeight);

  await ask.getByRole('button', { name: 'Allow for this session' }).click();
  await expect.poll(() => answers(requests).length).toBe(1);
  expect(answers(requests)[0]?.path).toBe(`/api/tracks/${TRACK.id}/asks/12/answer`);
  expect(answers(requests)[0]?.body).toEqual({ answers: [{ option: 1 }] });
  await expect.element(ask).not.toBeInTheDocument();
});

it('says a request that went away is no longer pending', async () => {
  await page.viewport(1280, 800);
  const { requests } = setup({ mode: 'ask', asks: [HOLD],
    answer: () => answered(409, { code: 'conflict', error: 'ask 12 is no longer open: its paused request is gone' }) });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await ask.getByRole('button', { name: 'Allow', exact: true }).click();
  await expect.poll(() => answers(requests).length).toBe(1);
  await expect.element(ask.getByText('This request is no longer pending.')).toBeVisible();
  expect(page.getByText(/its paused request is gone/).query()).toBeNull();
  await expect.element(ask.getByRole('heading', { name: HOLD_TITLE })).toBeVisible();
});

it('shows a paused turn\'s request ahead of an older question, since the turn is blocked on it', async () => {
  await page.viewport(1280, 800);
  const wake = { source: 'ask', key: 'ask:7', text: 'Which branch?', at_ms: 4, ask_id: 7,
    questions: [{ title: 'Which branch?', options: ['main', 'release'] }], delivery: 'wake' };
  setup({ mode: 'ask', asks: [HOLD, wake] });
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await expect.element(ask.getByText('Turn paused, waiting for your approval')).toBeVisible();
  await expect.element(ask.getByRole('heading', { name: HOLD_TITLE })).toBeVisible();
  await expect.element(page.getByRole('button', { name: '1 more ask' })).toBeVisible();
});
