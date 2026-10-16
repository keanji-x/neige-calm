import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); document.getElementById('root')?.remove(); });

it('offers the side menu only after the source mounts, while keeping the trigger text', async () => {
  const frames = new Map<number, FrameRequestCallback>();
  let sequence = 0;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { frames.set(++sequence, callback); return sequence; });
  vi.stubGlobal('cancelAnimationFrame', (id: number) => { frames.delete(id); });
  const fixture = renderDailyFixture({ initial: '/track/daily?panel=conversations', extraPlanner: true,
    reply: request => {
      if (request.path === '/api/version') return { status: 200, statusText: 'OK', body: {
        assistantProviders: true, conversationSide: true, webCompatVersion: 54, minWebCompatVersion: 54,
        syncEventVersion: 29, dbInstanceId: 'presentation-menu',
      } };
      if (request.path.includes('/second-planner/harness/items')) return { status: 200, statusText: 'OK', body: [{
        id: 1, worker_session_id: 'runtime', card_id: 'second-planner', track_id: 'daily', thread_id: 'thread',
        turn_id: null, turn_error_text: null, item_uuid: null, item_type: 'agentMessage', method: 'item/completed',
        params: JSON.stringify({ item: { text: 'Source ready for presentation.' } }), created_at_ms: 1,
      }] };
      return undefined;
    },
  });
  try {
    fireEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
    const field = await screen.findByRole('combobox', { name: 'Message' });
    await waitFor(() => expect(field.getAttribute('contenteditable')).toBe('true'));
    expect(document.querySelector('[data-nc-thread]')).toBeNull();
    await userEvent.type(field, '/');
    const menu = await screen.findByRole('listbox', { name: 'Commands' });
    const premature = within(menu).queryByRole('option', { name: /^side/ });
    if (premature !== null) await userEvent.click(premature);
    expect(field.textContent).toBe('/');
    expect(premature).toBeNull();
    for (let index = 0; index < 2; index++) {
      const callbacks = [...frames.values()];
      frames.clear();
      await act(async () => {
        for (const callback of callbacks) callback(0);
        await Promise.resolve();
      });
    }
    await screen.findByText('Source ready for presentation.');
    await userEvent.type(field, 's');
    await userEvent.click(await screen.findByRole('option', { name: /^side/ }));
    await screen.findByRole('region', { name: 'Side conversation · Codex' });
    expect(fixture.requests.filter(request => request.method === 'POST'
      && request.path.endsWith('/planner/input'))).toHaveLength(0);
  } finally { cleanup(); fixture.client.clear(); }
});
