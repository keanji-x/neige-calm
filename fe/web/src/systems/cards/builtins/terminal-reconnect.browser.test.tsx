import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';

import '../../../styles/entry.css';
import type { ClientMsg, DaemonMsg } from '../../terminal/generated-terminal.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { registerAvailableBuiltinCards } from './register.ts';

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
    open(text: string) {
      this.readyState = 1;
      this.onopen?.();
      this.message({ ServerHello: {
        protocol_version: 4, terminal_id: 'pty-1', session_id: 'session-1', client_role: 'Owner',
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

function mountTerminal(status: 'running' | 'starting' = 'running') {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const card = registry.resolve({ id: 'card-1', kind: 'terminal', payload: {},
    runtime: { runtime_id: 'run-1', kind: 'terminal', status, terminal_id: 'pty-1' },
  });
  if (card === null) throw new Error('Missing terminal');
  return render(<BoardHost host={createCardHost(registry)} items={[
    { card, title: 'Terminal', originalIndex: 0, deletable: true },
  ]} visible activeCardId="card-1" />);
}

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
  expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
  await page.screenshot({ path: 'test-results/terminal-disconnected.png' });
  await userEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
  await waitFor(() => expect(sockets).toHaveLength(2));
  expect(sockets[1].url).toBe(sockets[0].url);
  expect(screen.queryByRole('button', { name: 'Reconnect' })).toBeNull();
  act(() => sockets[1].open('restored prompt'));
  expect(await screen.findByRole('img', { name: 'status Working' })).toBeTruthy();
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
  expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'Reconnect' })).toBeNull();
});
