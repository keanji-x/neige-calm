import { historicalConversationPage, conversationCardId, historicalConversationRun } from './conversation-read-data.ts';
import { prepareReportDocument } from '../../features/report/document/preparation.ts';
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

function setup(theme: 'light' | 'dark', ask: typeof ASK = ASK, reopen = false) {
  const requests: ApiRequest[] = [];
  let currentTrack = reopen ? { ...TRACK, closed_at: 42 } : TRACK;
  const transport: ApiTransportPort = { async send(request) {
    await Promise.resolve();
    requests.push(request);
    let body: unknown = [];
    let status = 200;
    if (request.path === '/api/areas') body = [{ id: 'area', name: 'Work', color: '#123456', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }];
    if (request.path === '/api/areas/area/tracks') body = [currentTrack];
    if (request.path === '/api/settings') body = {};
    if (request.path === `/api/tracks/${TRACK.id}`) body = { track: currentTrack, can_reopen: currentTrack.closed_at !== null, can_admin: false, can_close: currentTrack.closed_at === null, cards: [PLANNER],
      overlays: [{ id: 'activity', plugin_id: 'kernel', entity_kind: 'track', entity_id: TRACK.id, kind: 'activity', updated_at: 5,
        payload: { schemaVersion: 4, working: false, attention: 'input', activity_at_ms: 5, items: [reopen ? { ...ask, action: { kind: 'reopen_track', closed_at: 42 }, questions: [{ title: 'Continue this closed track?', options: ['Reopen and continue', 'Keep closed'] }] } : ask], cards: [] } }] };
    if (request.path.includes('/planner/run?contract=conversation_v1')) body = historicalConversationRun({ card_id: PLANNER.id, worker_session_id: 'session', phase: 'idle',
      model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null, manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null, attachments_supported: true }, 'idle');
    if (request.path.endsWith('/harness/live')) body = { turn_id: null, items: [] };
    if (request.path.includes('/harness/items')) body = historicalConversationPage(conversationCardId(request.path), [{ id: 1, worker_session_id: 'session', card_id: PLANNER.id,
      track_id: TRACK.id, thread_id: 'thread', turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage',
      method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { id: 'reply', type: 'agentMessage', text: 'The release checklist is green. Two things before I tag it.' } }) }], null);
    if (request.path === `/api/tracks/${TRACK.id}/asks/7/answer`) { body = undefined; status = 204; if (reopen) currentTrack = { ...currentTrack, closed_at: null }; }
    if (request.path === `/api/cards/${PLANNER.id}/planner/attachments`) {
      body = { attachmentId: 'image-1.png', contentType: 'image/png', size: 68, url: PIXEL }; status = 201;
    }
    return { status, statusText: status === 204 ? 'No Content' : status === 201 ? 'Created' : 'OK', body };
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
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

it.each([1280, 390])('answers custom questions with Enter inside the real conversation at %ipx', async width => {
  await page.viewport(width, 844);
  const { requests } = setup('light');
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const message = page.getByRole('combobox', { name: 'Message' });
  await message.fill('Keep this message draft');
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  const branch = ask.getByRole('textbox', { name: 'Which branch should I release from?' });
  await branch.fill('hotfix');
  await branch.click();
  await userEvent.keyboard('{Enter}');
  expect(requests.filter(request => request.path.endsWith('/answer'))).toHaveLength(0);
  const notes = ask.getByRole('textbox', { name: 'Anything to tell the reviewers?' });
  await expect.element(notes).toHaveFocus();
  await notes.fill('Wait for CI');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.filter(request => request.path.endsWith('/answer')).length).toBe(1);
  expect(requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
  expect(requests.find(request => request.path.endsWith('/answer'))).toMatchObject({
    method: 'POST',
    path: `/api/tracks/${TRACK.id}/asks/7/answer`,
    body: { answers: [{ text: 'hotfix' }, { text: 'Wait for CI' }] },
  });
  await expect.element(ask).not.toBeInTheDocument();
  await expect.element(message).toHaveTextContent('Keep this message draft');
});

it('answers with Enter while keeping an image-only message draft unsent', async () => {
  await page.viewport(1280, 844);
  const { requests } = setup('light');
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const picker = document.querySelector<HTMLInputElement>('[data-nc-attach] input[type="file"]')!;
  await userEvent.upload(picker, new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'shot.png', { type: 'image/png' }));
  const image = page.getByRole('button', { name: 'Remove Image 1', exact: true });
  await expect.element(image).toBeVisible();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  const branch = ask.getByRole('textbox', { name: 'Which branch should I release from?' });
  await branch.fill('hotfix');
  await branch.click();
  await userEvent.keyboard('{Enter}');
  const notes = ask.getByRole('textbox', { name: 'Anything to tell the reviewers?' });
  await expect.element(notes).toHaveFocus();
  await notes.fill('Wait for CI');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => requests.find(request => request.path.endsWith('/answer'))?.body).toEqual({
    answers: [{ text: 'hotfix' }, { text: 'Wait for CI' }],
  });
  expect(requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
  await expect.element(image).toBeVisible();
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

it.each(['drop', 'paste'] as const)('attaches a clipboard or dragged image through %s in the real conversation', async (source) => {
  await page.viewport(1280, 800);
  const { requests } = setup('light');
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const field = await page.getByRole('combobox', { name: 'Message' }).findElement();
  await page.getByRole('combobox', { name: 'Message' }).click();
  const transfer = new DataTransfer();
  transfer.items.add(new File([await (await fetch(PIXEL)).arrayBuffer()], 'screenshot.png', { type: 'image/png' }));
  const event = source === 'drop'
    ? new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer })
    : new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer });
  field.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  await expect.poll(() => requests.filter(request => request.path.endsWith('/planner/attachments')).length).toBe(1);
  await expect.element(page.getByRole('button', { name: 'Remove Image 1', exact: true })).toBeVisible();
  await expect.poll(() => document.querySelector<HTMLImageElement>('[data-nc-attachments] img')?.naturalWidth).toBe(1);
  expect(requests.find(request => request.path.endsWith('/planner/attachments'))?.headers?.['content-type']).toBe('image/png');
  expect(field.textContent).toBe('');
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


it('retains an unanswered ask and its question across responsive surfaces', async () => {
  await page.viewport(1280, 800);
  const { requests } = setup('light');
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const ask = page.getByRole('group', { name: 'The Planner asks' });
  await ask.getByRole('button', { name: 'release/2.0' }).click();
  const own = ask.getByRole('textbox', { name: 'Anything to tell the reviewers?' });
  await own.fill('Keep the selected branch and this note.');
  const originalInput = await own.findElement();
  for (const width of [390, 1280]) {
    await page.viewport(width, 844);
    await expect.element(own).toHaveValue('Keep the selected branch and this note.');
    expect(await own.findElement()).toBe(originalInput);
    await expect.element(own).toHaveFocus();
    expect(requests.filter(request => request.path.endsWith('/answer'))).toHaveLength(0);
  }
  await ask.getByRole('button', { name: 'Answer' }).click();
  await expect.poll(() => requests.filter(request => request.path.endsWith('/answer')).length).toBe(1);
  expect(requests.find(request => request.path.endsWith('/answer'))?.body).toEqual({
    answers: [{ option: 1 }, { text: 'Keep the selected branch and this note.' }],
  });
});

it.each([1280, 390])('restores a closed track by clicking its existing AI question at %ipx', async (width) => {
  await page.viewport(width, 844);
  const { requests } = setup('light', ASK, true);
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  const option = page.getByRole('button', { name: 'Reopen and continue' });
  await expect.element(option).toBeVisible();
  await expect.element(page.getByRole('textbox', { name: 'Continue this closed track?' })).not.toBeInTheDocument();
  await page.screenshot({ path: `../../../../test-results/reopen-current-${width}.png` });
  await option.click();
  await expect.poll(() => requests.find(request => request.path.endsWith('/answer'))?.body).toEqual({ answers: [{ option: 0 }] });
  await expect.element(page.getByRole('status', { name: 'Track closed' })).not.toBeInTheDocument();
});

it('renders grant_admin choices and posts the grant click only to the ask answer route', async () => {
  const grantAsk = { ...ASK, action: { kind: 'grant_admin' },
    questions: [{ title: 'Manage Tracks?', options: ['Grant admin', 'Not now'] }] };
  const { requests } = setup('light', grantAsk);
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.element(page.getByRole('heading', { name: 'Manage Tracks?' })).toBeVisible();
  await expect.element(page.getByRole('button', { name: 'Not now' })).toBeVisible();
  await page.getByRole('button', { name: 'Grant admin' }).click();
  await expect.poll(() => requests.filter(request => request.path.endsWith('/asks/7/answer')).length).toBe(1);
  const answer = requests.find(request => request.path.endsWith('/asks/7/answer'));
  expect(answer?.path).toBe(`/api/tracks/${TRACK.id}/asks/7/answer`);
  expect(requests.some(request => request.method === 'PATCH')).toBe(false);
  expect(answer?.body).toEqual({ answers: [{ option: 0 }] });
});


it('marks a real pending question on its Planner conversation even without a per-card input verdict', async () => {
  await page.viewport(1400, 900);
  setup('light');
  const row = page.getByRole('button', { name: 'Conversation Planner', exact: true });
  await expect.element(row).toBeVisible();
  const button = row.element();
  expect(button.closest('li')?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity')).toBe('attention');
  const description = document.getElementById(button.getAttribute('aria-describedby') ?? '');
  expect(description?.textContent).toContain('Needs input');
});
