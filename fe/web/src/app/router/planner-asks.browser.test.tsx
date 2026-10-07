import '../../styles/entry.css';
// #2209 U3 in a real browser: a two-question ask above the Planner composer, answered and gone.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => {
  cleanup(); document.getElementById('root')?.remove(); delete document.documentElement.dataset.theme;
  await page.viewport(1280, 720);
});

const TRACK = { id: 'asks-track', area_id: 'area', title: 'Release 2.0', sort: 1, cwd: '/tmp',
  pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
const PLANNER = { id: 'planner', track_id: TRACK.id, kind: 'codex', title: 'Planner', sort: 1,
  payload: { planner_harness: true }, deletable: false, created_at: 1, updated_at: 2 };
const ASK = {
  source: 'ask', key: 'ask:7', text: 'Which branch should I release from? / Anything to tell the reviewers?', at_ms: 5, ask_id: 7,
  questions: [
    { title: 'Which branch should I release from?', options: ['main', 'release/2.0', 'A new branch from the last tag'] },
    { title: 'Anything to tell the reviewers?', options: [] },
  ],
  delivery: 'wake',
};

/* A 1×1 PNG, so the attached image's thumbnail draws without a server behind it. */
const PIXEL = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=';

/** An ask of `count` questions of `count` options each, `long` making every option the server's 200 characters. */
function largeAsk(count: number, long: boolean) {
  const questions = Array.from({ length: count }, (_, question) => ({
    title: `Question ${question + 1}: ${long ? 'which of these release plans should I follow, given what the checklist found? '.repeat(3) : 'which plan?'}`,
    options: Array.from({ length: count }, (_, option) => {
      const text = `Plan ${question + 1}.${option + 1} `;
      return long ? `${text}${'keep the branch, tag it, and write the notes before the docs land. '.repeat(4)}`.slice(0, 200) : text.trim();
    }),
  }));
  /* A short notification text: the kernel's joined titles would expand the Notifications aside over the conversation
     row this test opens, which is that aside's layout, not the drawer's. */
  return { ...ASK, key: 'ask:7', text: `${count} questions`, questions };
}

function setup(theme: 'light' | 'dark', ask: typeof ASK = ASK) {
  const requests: ApiRequest[] = [];
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [TRACK];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${TRACK.id}`) body = { track: TRACK, can_reopen: false, can_close: true, cards: [PLANNER],
      overlays: [{ id: 'activity', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity', updated_at: 5,
        payload: { schemaVersion: 4, working: false, attention: 'input', activity_at_ms: 5, items: [ask], cards: [] } }] };
    if (request.path.endsWith('/planner/run')) body = { card_id: PLANNER.id, worker_session_id: 'session', phase: 'idle',
      model: null, reasoning_effort: null, blocked_reason: null, running_turn: null, attachments_supported: true };
    if (request.path.endsWith('/harness/live')) body = { turn_id: null, items: [] };
    if (request.path.includes('/harness/items')) body = [{ id: 1, worker_session_id: 'session', card_id: PLANNER.id,
      track_id: TRACK.id, thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage',
      method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text: 'The release checklist is green. Two things before I tag it.' } }) }];
    if (request.path === `/api/tracks/${TRACK.id}/asks/7/answer`) { body = undefined; status = 204; }
    if (request.path === `/api/cards/${PLANNER.id}/planner/attachments`) {
      body = { attachmentId: 'image-1.png', contentType: 'image/png', size: 68, url: PIXEL }; status = 201;
    }
    return { status, statusText: status === 204 ? 'No Content' : status === 201 ? 'Created' : 'OK', body };
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

it.each([
  ['light', 1280], ['dark', 1280], ['light', 390], ['dark', 390],
] as const)('answers sequential questions inside the real Planner conversation in %s at %ipx', async (theme, width) => {
  await page.viewport(width, width < 600 ? 844 : 800);
  const { requests } = setup(theme);
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await expect.element(ask).toBeVisible();
  expect((await ask.findElement()).closest('[data-nc-composer]')).not.toBeNull();
  const questionElement = await ask.findElement();
  const answerLine = await ask.getByRole('group', { name: 'Your answer' }).findElement();
  expect(getComputedStyle(answerLine).borderBottomWidth).toBe('1px');
  expect(getComputedStyle(questionElement.querySelector('h3')!).fontSize).toBe('16px');
  expect(getComputedStyle(await ask.getByRole('button', { name: 'release/2.0' }).findElement()).fontSize).toBe('14px');

  await ask.getByRole('button', { name: 'release/2.0' }).click();
  expect(requests.filter(request => request.path.endsWith('/answer'))).toHaveLength(0);
  const own = ask.getByRole('textbox', { name: 'Anything to tell the reviewers?' });
  await expect.element(own).toHaveFocus();
  await own.fill('Tag it after the docs land.');
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  await ask.getByRole('button', { name: 'Answer' }).click();
  await expect.poll(() => requests.filter(request => request.path.endsWith('/answer')).length).toBe(1);
  const answer = requests.find(request => request.path.endsWith('/answer'));
  expect(answer?.method).toBe('POST');
  expect(answer?.path).toBe(`/api/tracks/${TRACK.id}/asks/7/answer`);
  expect(answer?.body).toEqual({ answers: [{ option: 1 }, { text: 'Tag it after the docs land.' }] });
  await expect.element(ask).not.toBeInTheDocument();
  await expect.element(page.getByRole('combobox', { name: 'Message' })).toBeVisible();
});

it('keeps attachments in the composer with questions in their own Astryx drawer', async () => {
  await page.viewport(1280, 800);
  setup('light');
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await expect.element(ask).toBeVisible();
  const picker = document.querySelector<HTMLInputElement>('[data-nc-attach] input[type="file"]')!;
  await userEvent.upload(picker, new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'shot.png', { type: 'image/png' }));
  await expect.poll(() => document.querySelectorAll('[data-nc-attachments] img').length).toBe(1);
  const strip = document.querySelector<HTMLElement>('[data-nc-attachments]')!;
  expect(strip.closest('[data-nc-composer]')).not.toBeNull();
  expect((await ask.findElement()).closest('[data-nc-composer]')).not.toBeNull();
});

it.each([
  [4, false, 1280, 720], [4, false, 390, 844],
  [8, true, 1280, 720], [8, true, 390, 844], [8, true, 844, 390],
] as const)('keeps the composer in view while answering %i questions (long=%s) at %i×%i', async (count, long, width, height) => {
  await page.viewport(width, height);
  const { requests } = setup('light', largeAsk(count, long));
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await expect.element(ask).toBeVisible();
  const field = (await page.getByRole('combobox', { name: 'Message' }).findElement()).getBoundingClientRect();
  expect(field.bottom).toBeLessThanOrEqual(window.innerHeight);
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
  expect((await ask.findElement()).closest('[data-nc-composer]')).not.toBeNull();
  for (let index = 0; index < count; index++) {
    await expect.element(ask.getByRole('heading', { name: new RegExp(`^Question ${index + 1}:`) })).toBeVisible();
    await ask.getByRole('button', { name: new RegExp(`^Plan ${index + 1}\\.1(?: |$)`) }).click();
  }
  await expect.poll(() => requests.filter(request => request.path.endsWith('/answer')).length).toBe(1);
  const answer = requests.find(request => request.path.endsWith('/answer'));
  expect(answer?.body).toEqual({ answers: largeAsk(count, long).questions.map(() => ({ option: 0 })) });
  await expect.element(ask).not.toBeInTheDocument();
});
