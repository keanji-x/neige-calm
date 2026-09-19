import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { RecoveryAccess } from '../../../../../core/domain/recovery/access.ts';

import '../../../styles/entry.css';
import type { ClientMsg, DaemonMsg } from '../../terminal/generated-terminal.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { registerAvailableBuiltinCards } from './register.ts';

// Load the real lazy chunk before timing socket behavior; cold Vite compilation
// belongs to fixture setup and can exceed the socket assertion's one-second wait.
beforeAll(async () => { await import('../../terminal/xterm-view.tsx'); });

function terminalTransport() {
  const sockets: Socket[] = [];
  class Socket {
    static OPEN = 1;
    static CONNECTING = 0;
    readyState = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: ((event: { code: number; reason: string; wasClean: boolean }) => void) | null = null;
    onerror: (() => void) | null = null;
    sent: ClientMsg[] = [];
    readonly url: string;
    constructor(url: string) { this.url = url; sockets.push(this); }
    send(data: string) { this.sent.push(JSON.parse(data) as ClientMsg); }
    close() { this.readyState = 3; }
    disconnect() {
      this.readyState = 3;
      this.onclose?.({ code: 1006, reason: '', wasClean: false });
    }
    open(text: string, role: 'Owner' | 'Observer' = 'Owner') {
      this.readyState = 1;
      this.onopen?.();
      this.message({ ServerHello: {
        protocol_version: 4, terminal_id: 'pty-1', session_id: 'session-1', client_role: role,
        owner_client_id: null, pty_size: { cols: 80, rows: 24, pixel_width: null, pixel_height: null },
        pty_seq_head: 1, pty_seq_tail: 0, render_rev: 1, history_gap: null, is_child_ready: true,
        snapshot: { cols: 80, rows: 24, render_rev: 1, pty_seq: 1, encoding: 'Vt',
          data: [...new TextEncoder().encode(text)], scrollback: null },
      } });
    }
    message(message: DaemonMsg) { this.onmessage?.({ data: JSON.stringify(message) }); }
  }
  vi.stubGlobal('WebSocket', Socket);
  return sockets;
}

function mountTerminal(status: 'running' | 'starting' = 'running', width?: number, recovery?: RecoveryAccess) {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const card = registry.resolve({ id: 'card-1', kind: 'terminal', payload: {},
    runtime: { worker_session_id: 'run-1', kind: 'terminal', status, terminal_id: 'pty-1' },
  });
  if (card === null) throw new Error('Missing terminal');
  return render(<div style={{ width }}><BoardHost host={createCardHost(registry, { recovery })} items={[
    { card, title: 'Terminal', originalIndex: 0, deletable: true, activity: null },
  ]} visible activeCardId="card-1" onRemoveCard={() => {}} /></div>);
}

/* The head's activity indicator. Its absence after a successful connect is the
   assertion that matters here (#1722 §5.3, INV-APP-118): the kernel's verdict
   is `BoardHostItem.activity`, `null` in every mount below, and a socket
   coming up must not paint one — the connection is this tab's, not the
   card's work. */
const headIndicator = () => document.querySelector('[data-nc-card-cell] [data-nc-activity]');
/* The head's words. `null` is the connected state and nothing else: attached and
   not ended, only `connected` prints no text (`Connecting…` / `Disconnected` /
   `Connection error` are the other three) — so "no text" is the positive
   evidence that the socket came up before the indicator is asserted absent. */
const headStatusText = () => document.querySelector('[data-nc-card-drag] [role="status"]')?.textContent ?? null;

function buffer() {
  return (window as unknown as { __xtermDumps__?: Record<string, () => string> }).__xtermDumps__?.['pty-1']?.() ?? '';
}

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('shows a refused connection and reconnects the same terminal without creating a process', async () => {
  await page.viewport(1200, 800);
  const sockets = terminalTransport();
  const mounted = mountTerminal();
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].disconnect());
  expect(await screen.findByText('Disconnected')).toBeTruthy();
  expect(headIndicator()).toBeNull();
  await page.screenshot({ path: 'test-results/terminal-disconnected.png' });
  await userEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
  await waitFor(() => expect(sockets).toHaveLength(2));
  expect(sockets[1].url).toBe(sockets[0].url);
  expect(screen.queryByRole('button', { name: 'Reconnect' })).toBeNull();
  act(() => sockets[1].open('restored prompt'));
  await waitFor(() => expect(headStatusText()).toBeNull());
  expect(headIndicator()).toBeNull();
  act(() => sockets[0].message({ ProtocolError: { code: 'NotOwner', message: 'Stale connection', expected_version: null } }));
  expect(headStatusText()).toBeNull();
  expect(headIndicator()).toBeNull();
  expect(sockets[1].sent.some((frame) => typeof frame === 'object' && 'ClientHello' in frame
    && frame.ClientHello.initial_scrollback === 'All')).toBe(true);
  mounted.unmount();
  expect(sockets.every((socket) => socket.readyState === 3 && socket.onmessage === null && socket.onclose === null)).toBe(true);
});

it('holds terminal output through failed reconnects and replaces it only with authoritative replay', async () => {
  const previousUrl = location.href;
  history.replaceState(null, '', `${location.pathname}?testMounts=1`);
  try {
    await page.viewport(1200, 800);
    const sockets = terminalTransport();
    mountTerminal();
    await waitFor(() => expect(sockets).toHaveLength(1));
    act(() => sockets[0].open('kept output'));
    await waitFor(() => expect(buffer()).toContain('kept output'));
    act(() => sockets[0].disconnect());
    expect(buffer()).toContain('kept output');
    await userEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
    await waitFor(() => expect(sockets).toHaveLength(2));
    expect(buffer()).toContain('kept output');
    act(() => sockets[1].disconnect());
    expect(buffer()).toContain('kept output');
    await userEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
    await waitFor(() => expect(sockets).toHaveLength(3));
    act(() => sockets[2].open('kept output\r\nnew output'));
    await waitFor(() => expect(buffer()).toContain('new output'));
    expect(buffer().match(/kept output/g)).toHaveLength(1);
    expect(sockets.flatMap((socket) => socket.sent).filter((frame) => typeof frame === 'object' && 'Input' in frame)).toEqual([]);
    await page.screenshot({ path: 'test-results/terminal-reconnected.png' });
  } finally {
    history.replaceState(null, '', previousUrl);
  }
});

it.each(['running', 'starting'] as const)('retains process exit truth when REST still says %s', async (status) => {
  await page.viewport(1200, 800);
  const sockets = terminalTransport();
  mountTerminal(status);
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => {
    sockets[0].open('finished');
    sockets[0].message({ TerminalExited: { code: 23, pty_seq: 2, render_rev: 2 } });
    sockets[0].disconnect();
  });
  expect(await screen.findByText('Session exited.')).toBeTruthy();
  expect(headIndicator()).toBeNull();
  expect(screen.queryByRole('button', { name: 'Reconnect' })).toBeNull();
});


it('accepts input and later automatically reconnects after a recoverable ownership rejection and successful owner claim', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const access = new RecoveryAccess(); access.change('connected');
  await page.viewport(1200, 800);
  const sockets = terminalTransport();
  mountTerminal('running', undefined, access);
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].open('observer prompt', 'Observer'));
  const hello = sockets[0].sent.find((frame) => typeof frame === 'object' && 'ClientHello' in frame);
  if (typeof hello !== 'object' || !('ClientHello' in hello)) throw new Error('ClientHello missing');
  act(() => {
    sockets[0].message({ ProtocolError: { code: 'NotOwner', message: 'Only the owner may resize', expected_version: null } });
    sockets[0].message({ OwnerChanged: { owner_client_id: hello.ClientHello.client_id } });
  });
  await userEvent.click(screen.getByLabelText('Terminal input'));
  await userEvent.keyboard('x');
  await waitFor(() => expect(sockets[0].sent.filter((frame) => typeof frame === 'object' && 'Input' in frame)).toHaveLength(1));
  expect(screen.queryByText('Connection error')).toBeNull();
  expect(headStatusText()).toBeNull();
  expect(headIndicator()).toBeNull();
  act(() => access.invalidate('recovering')); act(() => access.change('connected'));
  await waitFor(() => expect(sockets).toHaveLength(2));
  expect(sockets[1].url).toBe(sockets[0].url);
});

it.each(['UnsupportedVersion', 'UnsupportedEncoding', 'BadHandshake', 'BadSequence', 'SnapshotMissing', 'closed', 'exited'] as const)(
  'does not revive input after %s when a late owner notification arrives', async (failure) => {
    vi.stubGlobal('__NC_BUNDLED__', true);
    const access = new RecoveryAccess(); access.change('connected');
    await page.viewport(1200, 800);
    const sockets = terminalTransport();
    mountTerminal('running', undefined, access);
    await waitFor(() => expect(sockets).toHaveLength(1));
    act(() => sockets[0].open('observer prompt', 'Observer'));
    const hello = sockets[0].sent.find((frame) => typeof frame === 'object' && 'ClientHello' in frame);
    if (typeof hello !== 'object' || !('ClientHello' in hello)) throw new Error('ClientHello missing');
    act(() => {
      sockets[0].message({ ProtocolError: { code: 'NotOwner', message: 'Only the owner may resize', expected_version: null } });
      if (failure === 'closed') sockets[0].disconnect();
      else if (failure === 'exited') sockets[0].message({ TerminalExited: { code: 0, pty_seq: 2, render_rev: 2 } });
      else sockets[0].message({ ProtocolError: { code: failure, message: 'Cannot continue this connection', expected_version: null } });
      sockets[0].message({ OwnerChanged: { owner_client_id: hello.ClientHello.client_id } });
    });
    // Trigger the real input surface without clicking through the fatal-error overlay.
    const input = screen.getByLabelText('Terminal input');
    input.focus();
    await userEvent.keyboard('x');
    expect(sockets[0].sent.filter((frame) => typeof frame === 'object' && 'Input' in frame)).toHaveLength(0);
    expect(headIndicator()).toBeNull();
    if (failure === 'closed') expect(screen.getByRole('button', { name: 'Reconnect' })).toBeTruthy();
    if (failure !== 'closed' && failure !== 'exited') expect(screen.getByRole('alert').textContent).toContain('Cannot continue this connection');
    act(() => access.invalidate('recovering')); act(() => access.change('connected'));
    expect(sockets).toHaveLength(1);
  },
);

it('keeps the terminal title and disconnected status separate in a 171px card', async () => {
  await page.viewport(390, 844);
  const sockets = terminalTransport();
  mountTerminal('running', 356);
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].disconnect());
  await screen.findByText('Disconnected');
  const title = screen.getByText('Terminal');
  const status = screen.getByText('Disconnected');
  const card = document.querySelector<HTMLElement>('[data-nc-card-cell]')!;
  expect(card.getBoundingClientRect().width).toBeCloseTo(171, 0);
  const titleRect = title.getBoundingClientRect();
  const statusRect = status.getBoundingClientRect();
  expect(statusRect.left >= titleRect.right || statusRect.top >= titleRect.bottom).toBe(true);
  expect(statusRect.left).toBeGreaterThanOrEqual(card.getBoundingClientRect().left);
  expect(statusRect.right).toBeLessThanOrEqual(card.getBoundingClientRect().right);
  const closeRect = screen.getByRole('button', { name: 'Delete card Terminal' }).getBoundingClientRect();
  expect(statusRect.right <= closeRect.left || statusRect.top >= closeRect.bottom).toBe(true);
  await page.screenshot({ path: 'test-results/terminal-disconnected-narrow.png' });
});


it('keeps the disconnected status on the title row when the terminal is wide', async () => {
  await page.viewport(1200, 800);
  const sockets = terminalTransport();
  mountTerminal();
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].disconnect());
  const status = await screen.findByText('Disconnected');
  const titleRect = screen.getByText('Terminal').getBoundingClientRect();
  const statusRect = status.getBoundingClientRect();
  expect(statusRect.top).toBeCloseTo(titleRect.top, 0);
  expect(statusRect.left).toBeGreaterThanOrEqual(titleRect.right);
});


it('bundled recovery fences input and automatically reattaches the same terminal only after synchronization', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(390, 844);
  const access = new RecoveryAccess(); access.change('syncing'); const sockets = terminalTransport();
  mountTerminal('running', 390, access);
  await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  expect(sockets).toHaveLength(0);
  act(() => access.change('connected')); await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].open('old retained prompt'));
  await waitFor(() => expect(headStatusText()).toBeNull());
  expect(headIndicator()).toBeNull();
  act(() => access.invalidate('recovering'));
  expect(sockets[0].readyState).toBe(3);
  const textarea = document.querySelector<HTMLTextAreaElement>('[data-nc-terminal-id] textarea')!;
  await userEvent.type(textarea, 'offline input');
  act(() => access.change('syncing')); expect(sockets).toHaveLength(1);
  act(() => access.change('connected')); await waitFor(() => expect(sockets).toHaveLength(2));
  expect(sockets[1].url).toBe(sockets[0].url); act(() => sockets[1].open('authoritative prompt'));
  expect(sockets[1].sent.some(frame => typeof frame === 'object' && 'Input' in frame)).toBe(false);
  act(() => sockets[1].message({ ProtocolError: { code: 'NotOwner', message: 'Refused', expected_version: null } }));
  act(() => { access.invalidate('recovering'); access.change('connected'); });
  expect(sockets).toHaveLength(2);
});

it('a successful manual same-terminal reconnect starts a fresh automatic recovery episode', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(1200, 800);
  const access = new RecoveryAccess(); access.change('connected'); const sockets = terminalTransport();
  mountTerminal('running', undefined, access); await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => sockets[0].open('initial prompt'));
  act(() => { sockets[0].readyState = 3; sockets[0].onclose?.({ code: 1008, reason: 'permission refused', wasClean: true }); });
  await userEvent.click(await screen.findByRole('button', { name: 'Reconnect' }));
  await waitFor(() => expect(sockets).toHaveLength(2)); act(() => sockets[1].open('manual recovery succeeded'));
  act(() => { access.invalidate('recovering'); access.change('connected'); });
  await waitFor(() => expect(sockets).toHaveLength(3));
  expect(sockets[2].url).toBe(sockets[0].url);
  expect(sockets[2].sent.some(frame => typeof frame === 'object' && 'Input' in frame)).toBe(false);
});
it.each([1000, 1005])('a locally timed-out OPEN handshake retries the same terminal after normal close %s', async (code) => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(1200, 800);
  const deadlines: (() => void)[] = []; const schedule = globalThis.setTimeout.bind(globalThis);
  const clock = vi.spyOn(globalThis, 'setTimeout').mockImplementation((handler, delay) => {
    if (delay === 15_000 && typeof handler === 'function') deadlines.push(handler as () => void);
    return schedule(handler, delay);
  });
  try {
    const access = new RecoveryAccess(); access.change('connected'); const sockets = terminalTransport();
    mountTerminal('running', undefined, access); await waitFor(() => expect(sockets).toHaveLength(1));
    act(() => { sockets[0].readyState = 1; sockets[0].onopen?.(); });
    expect(deadlines).toHaveLength(1);
    act(() => { deadlines[0](); sockets[0].onclose?.({ code, reason: '', wasClean: true }); });
    await waitFor(() => expect(sockets).toHaveLength(2), { timeout: 2000 });
    expect(sockets[1].url).toBe(sockets[0].url);
    expect(sockets[0].sent.some(frame => typeof frame === 'object' && ('Input' in frame || 'ResizeCommit' in frame))).toBe(false);
  } finally { clock.mockRestore(); }
});
