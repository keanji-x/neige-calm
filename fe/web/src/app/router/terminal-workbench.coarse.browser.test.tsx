import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import { setupWorkbench, terminalCard } from './terminal-workbench-fixture.tsx';
import { workbenchSockets } from './terminal-workbench-socket-fixture.ts';
import '../../styles/entry.css';

beforeAll(async () => { await import('../../systems/terminal/xterm-view.tsx'); });
afterEach(() => { cleanup(); document.getElementById('root')?.remove(); vi.unstubAllGlobals(); });

it('hides phone entries and rejects manual commands locally while keeping the draft', async () => {
  await page.viewport(414, 896);
  const fixture = setupWorkbench();
  await page.getByRole('textbox', { name: 'Chat message' }).click();
  const composer = page.getByRole('combobox', { name: 'Message' });
  expect(screen.queryByRole('button', { name: 'Terminal workbench' })).toBeNull();
  await composer.fill('/');
  expect(screen.queryByRole('option', { name: /terminal/u })).toBeNull();
  for (const text of ['/terminal', '/terminal echo unsafe']) {
    await composer.fill(text); await composer.click(); await userEvent.keyboard('{Enter}');
    await expect.element(page.getByText('Terminal workbench is available on desktop.')).toBeVisible();
    await expect.element(composer).toHaveTextContent(text);
  }
  expect(document.querySelector('[data-nc-terminal-card]')).toBeNull();
  // The stationary dock's native form boundary must refuse the same command too.
  await page.getByRole('button', { name: 'Close conversation', exact: true }).click();
  const dock = await screen.findByRole<HTMLTextAreaElement>('textbox', { name: 'Chat message' });
  fireEvent.change(dock, { target: { value: '/terminal' } });
  fireEvent.submit(dock.closest('form')!);
  expect(dock.value).toBe('/terminal');
  // Opening history still acknowledges reading; neither command creates or sends anything.
  expect(fixture.requests.filter(request => request.method === 'POST' && !request.path.startsWith('/api/read-receipts/'))).toEqual([]);
});

it('preserves the ordinary phone terminal card input and confirmed deletion path', async () => {
  await page.viewport(414, 896);
  const { sockets, Socket } = workbenchSockets(); vi.stubGlobal('WebSocket', Socket);
  const fixture = setupWorkbench({ cards: [{ ...terminalCard('phone'), payload: { terminal_id: 'pty-1' } }] });
  await screen.findByRole<HTMLTextAreaElement>('textbox', { name: 'Chat message' });
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' }, search: { card: 'phone' } }); });
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].open());
  const input = await screen.findByRole('textbox', { name: 'Terminal input' });
  input.focus(); await userEvent.keyboard('pwd');
  expect(sockets[0].sent.some(frame => typeof frame === 'object' && 'Input' in frame)).toBe(true);
  expect(document.querySelectorAll('[data-nc-terminal-card]')).toHaveLength(1);
  await page.getByRole('button', { name: 'Delete card phone' }).click();
  const dialog = screen.getByRole('dialog', { name: 'Delete this card?' });
  expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(0);
  await userEvent.click(within(dialog).getByRole('button', { name: 'Delete card' }));
  await waitFor(() => expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(1));
  expect(fixture.posts()).toHaveLength(0);
});

it('uses viewport width on a coarse desktop and hides the retained host without focus on compact transition', async () => {
  await page.viewport(1280, 900);
  const { sockets, Socket } = workbenchSockets(); vi.stubGlobal('WebSocket', Socket);
  const fixture = setupWorkbench({ cards: [{ ...terminalCard('shell'), payload: { terminal_id: 'pty-1' } }] });
  const mount = vi.spyOn(fixture.runtime.host, 'mount');
  await page.getByRole('button', { name: /^shell/ }).click();
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].open());
  const input = document.querySelector('[data-nc-terminal-input]');
  await page.viewport(414, 896);
  await waitFor(() => expect(screen.queryByRole('button', { name: 'Terminal workbench' })).toBeNull());
  expect(screen.queryByRole('textbox', { name: 'Terminal input' })).toBeNull();
  expect(document.querySelector('[data-nc-terminal-input]')).toBe(input);
  expect(document.querySelectorAll('[data-nc-terminal-card]')).toHaveLength(1);
  expect(sockets[0].readyState).toBe(1);
  await page.getByRole('textbox', { name: 'Chat message' }).click();
  const composer = page.getByRole('combobox', { name: 'Message' });
  await composer.fill('keep phone draft');
  await page.viewport(1280, 900);
  await expect.element(page.getByRole('button', { name: 'Hide terminal' })).toBeVisible();
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('shell');
  expect(document.activeElement).not.toBe(input);
  expect(document.querySelector('[data-nc-terminal-input]')).toBe(input);
  expect(mount).toHaveBeenCalledTimes(1);
  // Opening the existing compact board hands ownership over once, including after close.
  await page.viewport(414, 896);
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' }, search: { card: 'shell' } }); });
  await waitFor(() => expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="shell"]')).not.toBeNull());
  expect(document.querySelectorAll('[data-nc-terminal-card]')).toHaveLength(1);
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' } }); });
  expect(document.querySelectorAll('[data-nc-terminal-card]')).toHaveLength(1);
  await page.viewport(1280, 900);
  await expect.element(page.getByRole('button', { name: 'Hide terminal' })).toBeVisible();
  expect(document.querySelectorAll('[data-nc-terminal-card]')).toHaveLength(1);
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('shell');
  expect(fixture.requests.some(request => request.method === 'DELETE')).toBe(false);
  expect(fixture.posts()).toHaveLength(0);
});
