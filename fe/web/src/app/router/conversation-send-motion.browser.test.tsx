import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { trackConversationCardId } from '../../../../core/domain/conversation.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { DraftMessage } from '../../features/chat/thread/message-entry.tsx';
import '../../styles/entry.css';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const text = 'A message submitted while its request stays pending.';
const field = () => screen.getByRole('combobox', { name: 'Message' });
const outgoing = () => document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
const surface = () => outgoing().closest<HTMLElement>('[data-nc-entry]') ?? outgoing();

function mount({ persistSent = false, running = false }: { persistSent?: boolean; running?: boolean } = {}) {
  const replies: { promise: Promise<ApiTransportResponse | undefined>; resolve: (value: ApiTransportResponse | undefined) => void }[] = [];
  let posts = 0;
  let answers = 0;
  let storedText: string | null = null;
  let created: { id: string; trackId: string; title: null; kind: string; provider: string; state: null; updatedAt: number; lastTurnCompletedAt: null } | null = null;
  const responseAt = (index: number) => {
    if (replies[index] === undefined) {
      let resolve!: (value: ApiTransportResponse | undefined) => void;
      const promise = new Promise<ApiTransportResponse | undefined>(done => { resolve = done; });
      replies[index] = { promise, resolve };
    }
    return replies[index];
  };
  const userRow = (cardId: string, body: string) => ({
    id: 2, worker_session_id: 'runtime', card_id: cardId, track_id: 'daily', thread_id: 'thread',
    turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'userMessage', method: 'item/completed',
    params: JSON.stringify({ item: { type: 'userMessage', content: [{ text: body }] } }), created_at_ms: 2,
  });
  const view = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (request.path === '/api/version') return ok({ assistantProviders: true, conversationCreateModel: true,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 29, dbInstanceId: 'send-motion' });
    if (request.path.startsWith('/api/agent-providers/managed')) return ok([{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }]);
    if (request.path === '/api/cards/second-planner/planner/run') return ok({ card_id: 'second-planner', worker_session_id: 'runtime',
      phase: running ? 'turn_running' : 'idle', model: null, reasoning_effort: null, permission_mode: 'never', blocked_reason: null,
      manual_compact_supported: true, compact_outcome: null, context_usage_invalidated: false, running_turn: null });
    if (request.path.endsWith('/harness/live')) return ok({ turn_id: 'running-turn', items: [] });
    if (request.path.includes('/second-planner/harness/items')) return ok([{
      id: 1, worker_session_id: 'runtime', card_id: 'second-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Stored history remains still.' } }), created_at_ms: 1,
    }, ...(storedText === null ? [] : [userRow('second-planner', storedText)])]);
    if (created !== null && request.path === '/api/tracks/daily/conversations') return ok([created]);
    if (created !== null && request.path.includes(`/${created.id}/harness/items`)) return ok([userRow(created.id, text)]);
    if (request.method === 'POST' && (request.path.endsWith('/planner/input') || request.path.endsWith('/conversations'))) {
      return responseAt(posts++).promise.then(result => {
        if (result !== undefined) return result;
        if (request.path.endsWith('/conversations')) {
          const key = request.headers?.['Idempotency-Key'];
          if (key === undefined) throw new Error('The create request must carry its key.');
          created = { id: trackConversationCardId('daily', key), trackId: 'daily', title: null, kind: 'track-assistant', provider: 'codex', state: null, updatedAt: 2, lastTurnCompletedAt: null };
          return { status: 201, statusText: 'Created', body: created };
        }
        if (persistSent) {
          const body = request.body as { text: string };
          storedText = body.text;
        }
        return ok({ card_id: 'second-planner', worker_session_id: 'runtime' });
      });
    }
    return undefined;
  } });
  return { ...view, answer: (result?: ApiTransportResponse) => responseAt(answers++).resolve(result) };
}
async function open(view: ReturnType<typeof mount>, draft = false) {
  if (draft) {
    fireEvent.click(await screen.findByRole('button', { name: 'New conversation' }));
    await waitFor(() => expect(view.client.getQueryData(['server-version'])).toMatchObject({ assistantProviders: true }));
    await waitFor(() => expect(view.client.getQueryData(['managed-providers'])).toHaveLength(1));
  } else {
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
    await screen.findByText('Stored history remains still.', {}, { timeout: 5000 });
  }
  await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
  await act(async () => { field().textContent = text; fireEvent.input(field()); await Promise.resolve(); });
}
afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await commands.emulateReducedMotion(false); });

it.each([false, true])('starts a native local message animation before the held request answers (first message: %s)', async draft => {
  await page.viewport(1400, 900);
  const view = mount();
  try {
    await open(view, draft);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    expect(outgoing().textContent).toBe(text);
    expect(screen.getByText('Sending…')).toBeDefined();
    const animation = message.getAnimations()[0];
    expect(animation).toBeDefined();
    expect(animation.playState).toBe('running');
    const keyframes = (animation.effect as KeyframeEffect).getKeyframes();
    expect(keyframes[0].opacity).toBe('0');
    expect(keyframes.at(-1)?.opacity).toBe('1');
    expect(keyframes.some(frame => String(frame.transform).includes('scale'))).toBe(false);
    expect(document.querySelector('[data-nc-turn="agent"]')?.getAnimations()).toEqual(draft ? undefined : []);
    if (!draft) {
      // Visual arrival may finish while dispatch is still pending; it never settles the request.
      animation.finish();
      await expect.poll(() => message.getAnimations().length).toBe(0);
      expect(screen.getByText('Sending…')).toBeDefined();
      expect(outgoing().textContent).toBe(text);
    }
    // Leaving the pane releases even an unfinished local animation without waiting on the request.
    fireEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
    await expect.poll(() => message.getAnimations().length).toBe(0);
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('settles local message motion immediately under reduced motion and releases it when the preference changes', async () => {
  await page.viewport(1400, 900);
  const view = mount();
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    expect(message.getAnimations()).toHaveLength(1);
    await commands.emulateReducedMotion(true);
    await expect.poll(() => message.getAnimations().length).toBe(0);
    expect(getComputedStyle(message).opacity).toBe('1');
    expect(getComputedStyle(message).transform).toBe('none');
    expect(screen.getByText('Sending…')).toBeDefined();
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(surface()).toBe(message);
  } finally { cleanup(); view.answer(); view.client.clear(); }
  const reduced = mount();
  try {
    await open(reduced);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    expect(surface().getAnimations()).toHaveLength(0);
    expect(outgoing().textContent).toBe(text);
    expect(screen.getByText('Sending…')).toBeDefined();
  } finally { cleanup(); reduced.answer(); reduced.client.clear(); }
});

it('finishes the same native entry animation independently after the real send succeeds', async () => {
  await page.viewport(1400, 900);
  const view = mount();
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    const animation = message.getAnimations()[0];
    expect(animation).toBeDefined();
    animation.pause();
    animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(surface()).toBe(message);
    expect(message.getAnimations()).toContain(animation);
    expect(animation.playState).toBe('paused');
    animation.play();
    await expect.poll(() => message.getAnimations().length).toBe(0);
    expect(getComputedStyle(message).transform).toBe('none');
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('keeps the same draft surface animation when its sending flag ends', async () => {
  const view = render(<DraftMessage text={text} sending />);
  const message = outgoing();
  const animation = message.getAnimations()[0];
  expect(animation).toBeDefined();
  animation.pause();
  animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
  view.rerender(<DraftMessage text={text} sending={false} />);
  expect(outgoing()).toBe(message);
  expect(message.getAnimations()).toContain(animation);
  expect(animation.playState).toBe('paused');
  animation.play();
  await expect.poll(() => message.getAnimations().length).toBe(0);
});

it('lets a refused first message finish its arrival on the retained draft surface', async () => {
  const view = mount();
  try {
    await open(view, true);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    const animation = message.getAnimations()[0];
    animation.pause();
    animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
    await act(async () => {
      view.answer({ status: 429, statusText: 'Too Many Requests', body: { code: 'rate_limited', error: 'Wait before retrying.' } });
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(screen.getByRole('button', { name: 'Try again' })).toBeDefined();
    expect(surface()).toBe(message);
    expect(message.getAnimations()).toContain(animation);
    animation.play();
    await expect.poll(() => message.getAnimations().length).toBe(0);
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('releases the old physical draft on real adoption without retaining or replaying its replacement', async () => {
  const view = mount();
  try {
    await open(view, true);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    const animation = message.getAnimations()[0];
    animation.pause();
    animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
    await act(async () => { view.answer(); await Promise.resolve(); });
    await expect.poll(() => message.isConnected).toBe(false);
    expect(message.getAnimations()).toHaveLength(0);
    await screen.findByText(text, {}, { timeout: 5000 });
    expect(surface()).not.toBe(message);
    expect(surface().getAnimations()).toHaveLength(0);
    expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(1);
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('releases an optimistic surface when canonical history actually replaces it', async () => {
  const view = mount({ persistSent: true });
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    const animation = message.getAnimations()[0];
    animation.pause();
    animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
    await act(async () => { view.answer(); await Promise.resolve(); });
    await expect.poll(() => message.isConnected).toBe(false);
    expect(message.getAnimations()).toHaveLength(0);
    expect(surface()).not.toBe(message);
    expect(surface().getAnimations()).toHaveLength(0);
    expect(document.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(1);
  } finally { cleanup(); view.answer(); view.client.clear(); }
});

it('starts another queued outgoing surface while the successful first arrival still runs', async () => {
  const view = mount({ running: true });
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const first = surface();
    const firstAnimation = first.getAnimations()[0];
    firstAnimation.pause();
    firstAnimation.currentTime = Number(firstAnimation.effect!.getTiming().duration) / 4;
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(field().getAttribute('contenteditable')).toBe('true'));
    await act(async () => { field().textContent = 'Another queued message'; fireEvent.input(field()); await Promise.resolve(); });
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const second = document.querySelectorAll<HTMLElement>('[data-nc-turn="you"]')[1].closest<HTMLElement>('[data-nc-entry]')!;
    const secondAnimation = second.getAnimations()[0];
    expect(secondAnimation).toBeDefined();
    expect(secondAnimation).not.toBe(firstAnimation);
    expect(first.getAnimations()).toContain(firstAnimation);
    expect(screen.getByText('Sending…')).toBeDefined();
    expect(screen.getByRole('button', { name: 'Stop' })).toBeDefined();
  } finally { cleanup(); view.answer(); view.client.clear(); }
});
