import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import type { ApiTransportResponse } from '../../../../core/api/types.ts';
import { WEB_COMPAT_VERSION } from '../providers/public.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import '../../styles/entry.css';

const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
const text = 'A message submitted while its request stays pending.';
const field = () => screen.getByRole('combobox', { name: 'Message' });
const outgoing = () => document.querySelector<HTMLElement>('[data-nc-turn="you"]')!;
const surface = () => outgoing().closest<HTMLElement>('[data-nc-entry]') ?? outgoing();

function mount() {
  let answer!: (response: ApiTransportResponse) => void;
  const response = new Promise<ApiTransportResponse>(resolve => { answer = resolve; });
  const view = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true, reply: request => {
    if (request.path === '/api/version') return ok({ assistantProviders: true, conversationCreateModel: true,
      webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 29, dbInstanceId: 'send-motion' });
    if (request.path.startsWith('/api/agent-providers/managed')) return ok([{
      availability: { provider: 'codex', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
      capabilities: { managed_conversation: true, images: true, manual_compact: true, history_replacement: true, requires_ready_availability: false },
    }]);
    if (request.path.includes('/second-planner/harness/items')) return ok([{
      id: 1, worker_session_id: 'runtime', card_id: 'second-planner', track_id: 'daily', thread_id: 'thread',
      turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
      params: JSON.stringify({ item: { text: 'Stored history remains still.' } }), created_at_ms: 1,
    }]);
    if (request.method === 'POST' && (request.path.endsWith('/planner/input') || request.path.endsWith('/conversations'))) return response;
    return undefined;
  } });
  return { ...view, answer: () => answer(ok({ card_id: 'second-planner', worker_session_id: 'runtime' })) };
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

it('releases a local entry animation as soon as its real send settles', async () => {
  await page.viewport(1400, 900);
  const view = mount();
  try {
    await open(view);
    act(() => { fireEvent.keyDown(field(), { key: 'Enter' }); });
    const message = surface();
    expect(message.getAnimations()).toHaveLength(1);
    await act(async () => { view.answer(); await Promise.resolve(); });
    await waitFor(() => expect(screen.queryByText('Sending…')).toBeNull());
    expect(message.getAnimations()).toHaveLength(0);
    expect(surface()).toBe(message);
    expect(getComputedStyle(message).transform).toBe('none');
  } finally { cleanup(); view.answer(); view.client.clear(); }
});
