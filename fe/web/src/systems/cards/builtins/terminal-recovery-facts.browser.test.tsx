import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import '../../../styles/entry.css';
import { RecoveryAccess } from '../../../../../core/domain/recovery/access.ts';
import type { DaemonMsg } from '../../terminal/generated-terminal.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { registerAvailableBuiltinCards } from './register.ts';
import { XtermView, type XtermViewHandle } from '../../terminal/xterm-view.tsx';
import { createRef } from 'react';

beforeAll(async () => { await import('../../terminal/xterm-view.tsx'); });
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each(['exit', 'fatal-protocol'] as const)('retains %s truth after a recovery pause', async kind => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  await page.viewport(390, 844);
  const sockets: Socket[] = [];
  class Socket {
    static OPEN = 1;
    static CONNECTING = 0;
    readyState = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    constructor() { sockets.push(this); }
    send() {}
    close() { this.readyState = 3; }
    message(message: DaemonMsg) { this.onmessage?.({ data: JSON.stringify(message) }); }
  }
  vi.stubGlobal('WebSocket', Socket);
  const access = new RecoveryAccess(); access.change('connected');
  const registry = createCardRegistry(); registerAvailableBuiltinCards(registry);
  const card = registry.resolve({ id: 'card-1', kind: 'terminal', payload: {},
    runtime: { worker_session_id: 'run-1', kind: 'terminal', status: 'running', terminal_id: 'pty-1' } });
  if (!card) throw new Error('Fixture terminal missing');
  render(<div style={{ width: 390 }}><BoardHost host={createCardHost(registry, { recovery: access })}
    items={[{ card, title: 'Terminal', originalIndex: 0, deletable: true, activity: null }]}
    visible activeCardId="card-1" onRemoveCard={() => {}} /></div>);
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => {
    sockets[0].readyState = 1; sockets[0].onopen?.();
    sockets[0].message({ ServerHello: { protocol_version: 4, terminal_id: 'pty-1', session_id: 's',
      client_role: 'Owner', owner_client_id: null, pty_size: { cols: 80, rows: 24, pixel_width: null, pixel_height: null },
      pty_seq_head: 1, pty_seq_tail: 0, render_rev: 1, history_gap: null, is_child_ready: true,
      snapshot: { cols: 80, rows: 24, render_rev: 1, pty_seq: 1, encoding: 'Vt', data: [], scrollback: null } } });
    sockets[0].message(kind === 'exit' ? { TerminalExited: { code: 0, pty_seq: 1, render_rev: 1 } }
      : { ProtocolError: { code: 'UnsupportedVersion', message: 'Incompatible protocol', expected_version: 5 } });
  });
  if (kind === 'exit') await screen.findByText('Session exited.');
  else expect((await screen.findByRole('alert')).textContent).toContain('UnsupportedVersion');
  act(() => access.invalidate('recovering'));
  act(() => access.change('connected'));
  expect(sockets).toHaveLength(1);
  if (kind === 'exit') expect(screen.queryByText('Session exited.')).not.toBeNull();
  else {
    expect(screen.queryByRole('alert')?.textContent ?? '').toContain('UnsupportedVersion');
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'Reconnect' })).toBeNull();
  }
});

it('retains terminal exit details through repeated revocation until a deliberate attach', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  await page.viewport(800, 600);
  const sockets: Socket[] = [];
  class Socket {
    static OPEN = 1; static CONNECTING = 0; readyState = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null; onerror: (() => void) | null = null;
    constructor() { sockets.push(this); }
    send() {} close() { this.readyState = 3; }
  }
  vi.stubGlobal('WebSocket', Socket);
  const access = new RecoveryAccess(); access.change('connected');
  const exit = vi.fn(); const ref = createRef<XtermViewHandle>();
  render(<div style={{ width: 800, height: 400 }}><XtermView ref={ref} terminalId="pty" recovery={access} onExitChange={exit} /></div>);
  await waitFor(() => expect(sockets).toHaveLength(1));
  act(() => {
    sockets[0].readyState = 1; sockets[0].onopen?.();
    const hello: DaemonMsg = { ServerHello: { protocol_version: 4, terminal_id: 'pty', session_id: 's',
      client_role: 'Owner', owner_client_id: null, pty_size: { cols: 80, rows: 24, pixel_width: null, pixel_height: null },
      pty_seq_head: 1, pty_seq_tail: 0, render_rev: 1, history_gap: null, is_child_ready: true,
      snapshot: { cols: 80, rows: 24, render_rev: 1, pty_seq: 1, encoding: 'Vt', data: [], scrollback: null } } };
    sockets[0].onmessage?.({ data: JSON.stringify(hello) });
    sockets[0].onmessage?.({ data: JSON.stringify({ TerminalExited: { code: 137, pty_seq: 1, render_rev: 1 } }) });
  });
  expect(exit).toHaveBeenLastCalledWith({ exit_code: 137, signal_killed: false });
  const calls = exit.mock.calls.length;
  act(() => { access.invalidate('paused'); access.invalidate('recovering'); access.change('connected'); });
  expect(exit).toHaveBeenCalledTimes(calls); expect(sockets).toHaveLength(1);
  act(() => ref.current?.refresh());
  await waitFor(() => expect(sockets).toHaveLength(2));
  expect(exit).toHaveBeenLastCalledWith(null);
});
