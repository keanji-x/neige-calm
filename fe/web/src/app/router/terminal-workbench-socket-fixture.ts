import type { ClientMsg, DaemonMsg } from '../../systems/terminal/generated-terminal.ts';

/** Wire simulator only; real xterm, input, fit and lifecycle execute in browser tests. */
export function workbenchSockets() {
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
    open() {
      this.readyState = 1; this.onopen?.();
      this.message({ ServerHello: { protocol_version: 4, terminal_id: 'pty-1', session_id: 'session-1', client_role: 'Owner', owner_client_id: null,
        pty_size: { cols: 80, rows: 24, pixel_width: null, pixel_height: null }, pty_seq_head: 1, pty_seq_tail: 0, render_rev: 1, history_gap: null, is_child_ready: true,
        snapshot: { cols: 80, rows: 24, render_rev: 1, pty_seq: 1, encoding: 'Vt', data: [...new TextEncoder().encode('workbench prompt')], scrollback: null } } });
    }
    message(message: DaemonMsg) { this.onmessage?.({ data: JSON.stringify(message) }); }
  }
  return { sockets, Socket };
}
