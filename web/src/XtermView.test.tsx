// Unit tests for the v2 terminal protocol wiring in XtermView.
//
// We don't render real xterm.js — `@xterm/xterm` needs a real canvas and
// `term.fit()` depends on layout that jsdom doesn't compute. Instead we
// mock the terminal constructor with a minimal stub that records the
// methods the component drives (`write`, `resize`, `clear`, `writeln`,
// `onData`). The wire-protocol side gets exercised via a `FakeWebSocket`
// captured by the global ctor (same pattern as `api/events.test.ts`).
//
// What's locked here:
//   - On WebSocket open, the component sends a `ClientHello` frame with
//     protocol_version=2, terminal_id, a `client_id` UUID, the local
//     viewport, `role_hint: 'Owner'`, and capabilities advertising `Vt`
//     encoding + scrollback support but no images.
//   - On `ServerHello`, the snapshot bytes are written to the term and
//     status flips to 'connected'.
//   - On `RenderPatch`, the patch bytes are written to the term.
//   - On `ResizeApplied`, no error is thrown and the response is taken
//     into account (epoch consumed). We treat this as a smoke test of the
//     dispatch — the epoch consumption itself is internal.
//   - On `ProtocolError`, the overlay surfaces the code+message and a
//     "Refresh" button appears.
//   - On `TerminalExited`, the overlay surfaces the exit code and a
//     "Restart" button appears.
//
// Live render tests for real PTYs belong in playwright e2e.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, act } from '@testing-library/react';

// ---- xterm mock --------------------------------------------------------

interface MockTerm {
  cols: number;
  rows: number;
  options: { theme?: unknown };
  write: ReturnType<typeof vi.fn>;
  writeln: ReturnType<typeof vi.fn>;
  clear: ReturnType<typeof vi.fn>;
  resize: ReturnType<typeof vi.fn>;
  open: ReturnType<typeof vi.fn>;
  loadAddon: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  onData: (cb: (d: string) => void) => { dispose: () => void };
  __dataCb?: (d: string) => void;
}

let mockTerm: MockTerm;

vi.mock('@xterm/xterm', () => {
  class Terminal {
    cols = 80;
    rows = 24;
    // The component mutates `term.options.theme` via the theme-apply
    // effect. Real xterm.js exposes `options` as a mutable bag; mirror
    // that here so writes don't throw.
    options: { theme?: unknown } = {};
    write = vi.fn();
    writeln = vi.fn();
    clear = vi.fn();
    resize = vi.fn((cols: number, rows: number) => {
      this.cols = cols;
      this.rows = rows;
    });
    open = vi.fn();
    loadAddon = vi.fn();
    dispose = vi.fn();
    onData(cb: (d: string) => void): { dispose: () => void } {
      // Expose the callback so a test can simulate typing.
      (this as unknown as MockTerm).__dataCb = cb;
      return { dispose: () => {} };
    }
    constructor(opts?: { theme?: unknown }) {
      if (opts?.theme !== undefined) this.options.theme = opts.theme;
      mockTerm = this as unknown as MockTerm;
    }
  }
  return { Terminal };
});

vi.mock('@xterm/addon-fit', () => {
  class FitAddon {
    fit = vi.fn();
  }
  return { FitAddon };
});

// Importing CSS from a .ts module under jsdom would error; tell vitest
// it's an empty module.
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

// ---- WebSocket mock ----------------------------------------------------

interface FakeWS {
  readyState: number;
  url: string;
  sentFrames: string[];
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: string }) => void) | null;
  onclose: ((ev: { code: number; reason: string; wasClean: boolean }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
  send: (data: string) => void;
  close: () => void;
  // Test helpers
  fireOpen: () => void;
  push: (json: unknown) => void;
  fireClose: (code?: number, reason?: string) => void;
}

let wsInstances: FakeWS[] = [];

class FakeWebSocketCtor {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readyState = FakeWebSocketCtor.CONNECTING;
  url: string;
  sentFrames: string[] = [];
  onopen: ((ev: unknown) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: ((ev: { code: number; reason: string; wasClean: boolean }) => void) | null = null;
  onerror: ((ev: unknown) => void) | null = null;
  constructor(url: string) {
    this.url = url;
    wsInstances.push(this as unknown as FakeWS);
  }
  send(data: string): void {
    this.sentFrames.push(data);
  }
  close(): void {
    this.readyState = FakeWebSocketCtor.CLOSED;
  }
  // Test helpers
  fireOpen(): void {
    this.readyState = FakeWebSocketCtor.OPEN;
    this.onopen?.({});
  }
  push(json: unknown): void {
    this.onmessage?.({ data: JSON.stringify(json) });
  }
  fireClose(code = 1006, reason = ''): void {
    this.readyState = FakeWebSocketCtor.CLOSED;
    this.onclose?.({ code, reason, wasClean: false });
  }
}

function currentWs(): FakeWS {
  const w = wsInstances[wsInstances.length - 1];
  if (!w) throw new Error('no FakeWebSocket constructed yet');
  return w;
}

// ---- import after mocks ------------------------------------------------

import { XtermView, LIGHT_THEME, DARK_THEME } from './XtermView';

beforeEach(() => {
  wsInstances = [];
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket =
    FakeWebSocketCtor as unknown as typeof WebSocket;
  // jsdom doesn't have ResizeObserver; the component installs one.
  (globalThis as { ResizeObserver: typeof ResizeObserver }).ResizeObserver =
    class {
      observe() {}
      disconnect() {}
      unobserve() {}
    } as unknown as typeof ResizeObserver;
  // jsdom doesn't have crypto.randomUUID by default in older versions.
  if (!('randomUUID' in (globalThis.crypto ?? {}))) {
    Object.defineProperty(globalThis.crypto, 'randomUUID', {
      configurable: true,
      value: () => '00000000-0000-4000-8000-000000000000',
    });
  }
});

afterEach(() => {
  wsInstances = [];
});

// Helper: build a minimal valid `ServerHello` for the daemon handshake.
function serverHello(over: Record<string, unknown> = {}): unknown {
  return {
    ServerHello: {
      protocol_version: 2,
      terminal_id: 'term_test',
      session_id: '11111111-1111-4111-8111-111111111111',
      client_role: 'Owner',
      owner_client_id: '00000000-0000-4000-8000-000000000000',
      pty_size: { cols: 80, rows: 24, pixel_width: null, pixel_height: null },
      pty_seq_head: 0,
      pty_seq_tail: 0,
      render_rev: 1,
      snapshot: {
        render_rev: 1,
        pty_seq: 0,
        cols: 80,
        rows: 24,
        encoding: 'Vt',
        data: [104, 105], // 'hi'
        scrollback: null,
      },
      history_gap: null,
      is_child_ready: false,
      ...over,
    },
  };
}

describe('XtermView v2 handshake', () => {
  it('sends ClientHello with protocol_version=2 and role_hint Owner on open', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    expect(ws.sentFrames).toHaveLength(1);
    const frame = JSON.parse(ws.sentFrames[0]!);
    expect(frame).toHaveProperty('ClientHello');
    const hello = frame.ClientHello;
    expect(hello.protocol_version).toBe(2);
    expect(hello.terminal_id).toBe('term_test');
    expect(typeof hello.client_id).toBe('string');
    expect(hello.role_hint).toBe('Owner');
    expect(hello.capabilities.render_encodings).toEqual(['Vt']);
    expect(hello.capabilities.supports_sixel).toBe(false);
    expect(hello.capabilities.supports_images).toBe(false);
    expect(hello.initial_scrollback).toBe('None');
    expect(hello.resume_from).toBeNull();
    expect(hello.desired_size.cols).toBe(80);
    expect(hello.desired_size.rows).toBe(24);
  });

  it("shows 'handshaking…' between WS open and ServerHello", () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    expect(screen.getByText(/handshaking/i)).toBeInTheDocument();
  });

  it('applies the snapshot data on ServerHello and transitions to connected', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    // 'hi' = [104, 105]
    expect(mockTerm.write).toHaveBeenCalled();
    const firstWriteArg = mockTerm.write.mock.calls[0]![0] as Uint8Array;
    expect(firstWriteArg).toBeInstanceOf(Uint8Array);
    expect(Array.from(firstWriteArg)).toEqual([104, 105]);
    // The 'connected' state hides the handshaking overlay.
    expect(screen.queryByText(/handshaking/i)).not.toBeInTheDocument();
  });

  it('resizes the local term if the snapshot size differs', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(
        serverHello({
          snapshot: {
            render_rev: 1,
            pty_seq: 0,
            cols: 132,
            rows: 50,
            encoding: 'Vt',
            data: [],
            scrollback: null,
          },
        }),
      );
    });
    expect(mockTerm.resize).toHaveBeenCalledWith(132, 50);
  });
});

describe('XtermView v2 streaming', () => {
  it('writes RenderPatch.data to the terminal', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    mockTerm.write.mockClear();
    act(() => {
      ws.push({
        RenderPatch: {
          render_rev: 2,
          prev_render_rev: 1,
          pty_seq: 5,
          encoding: 'Vt',
          data: [65, 66, 67], // 'ABC'
        },
      });
    });
    expect(mockTerm.write).toHaveBeenCalledTimes(1);
    const arg = mockTerm.write.mock.calls[0]![0] as Uint8Array;
    expect(Array.from(arg)).toEqual([65, 66, 67]);
  });

  it('clears and re-writes on a standalone RenderSnapshot', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    mockTerm.write.mockClear();
    mockTerm.clear.mockClear();
    act(() => {
      ws.push({
        RenderSnapshot: {
          render_rev: 3,
          pty_seq: 9,
          cols: 80,
          rows: 24,
          encoding: 'Vt',
          data: [88, 89],
          scrollback: null,
        },
      });
    });
    expect(mockTerm.clear).toHaveBeenCalledTimes(1);
    expect(mockTerm.write).toHaveBeenCalledTimes(1);
  });

  it('sends typed input as a v2 Input frame', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // Clear the ClientHello frame.
    ws.sentFrames.length = 0;
    // Simulate the user typing 'a'.
    expect(mockTerm.__dataCb).toBeDefined();
    act(() => {
      mockTerm.__dataCb!('a');
    });
    expect(ws.sentFrames).toHaveLength(1);
    const frame = JSON.parse(ws.sentFrames[0]!);
    expect(frame).toHaveProperty('Input');
    // Issue #115: browser typing path always emits `input_seq: 0`
    // ("no ack requested" — option (b)). The daemon writes the bytes
    // and stays silent — no `DaemonMsg::InputAck` on the hot path.
    expect(frame.Input).toEqual({ data: [97], input_seq: 0 }); // 'a' = 0x61
  });
});

describe('XtermView v2 terminal states', () => {
  it('shows protocol-error overlay on ProtocolError', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push({
        ProtocolError: {
          code: 'UnsupportedVersion',
          message: 'kernel is v3',
          expected_version: 3,
        },
      });
    });
    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(
      screen.getByText(/protocol error: UnsupportedVersion/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/kernel is v3/)).toBeInTheDocument();
    expect(screen.getByText(/refresh required for protocol v2/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /refresh/i })).toBeInTheDocument();
  });

  it('shows the exited overlay with the code on TerminalExited', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    act(() => {
      ws.push({
        TerminalExited: { code: 137, pty_seq: 10, render_rev: 5 },
      });
    });
    expect(screen.getByText(/process exited/i)).toBeInTheDocument();
    expect(screen.getByText(/code 137/)).toBeInTheDocument();
    expect(mockTerm.writeln).toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /restart/i })).toBeInTheDocument();
  });

  it('shows the disconnected overlay with close code on plain WS close', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    act(() => {
      ws.fireClose(1006, 'abnormal');
    });
    expect(screen.getByText(/disconnected/i)).toBeInTheDocument();
    expect(screen.getByText(/1006/)).toBeInTheDocument();
    expect(screen.getByText(/abnormal/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /reconnect/i })).toBeInTheDocument();
  });

  it('does not regress to disconnected when WS closes after a ProtocolError', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push({
        ProtocolError: {
          code: 'BadHandshake',
          message: 'malformed ClientHello',
          expected_version: null,
        },
      });
    });
    act(() => {
      ws.fireClose(1006, '');
    });
    // ProtocolError overlay should remain — it carries richer info than
    // the generic close code.
    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByText(/protocol error: BadHandshake/i)).toBeInTheDocument();
  });
});

// Helper: parse a CSS color spec (#rgb / #rrggbb / #rrggbbaa) into r,g,b,a.
// We deliberately only support the hex shapes the themes use so the test
// fails loudly if someone reintroduces an `rgba()` background.
function parseHex(s: string): { r: number; g: number; b: number; a: number } {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i.exec(s);
  if (!m) throw new Error(`unsupported color spec for this test: ${s}`);
  const h = m[1]!;
  const exp =
    h.length === 3
      ? h
          .split('')
          .map((c) => c + c)
          .join('') + 'ff'
      : h.length === 6
        ? h + 'ff'
        : h;
  return {
    r: parseInt(exp.slice(0, 2), 16),
    g: parseInt(exp.slice(2, 4), 16),
    b: parseInt(exp.slice(4, 6), 16),
    a: parseInt(exp.slice(6, 8), 16),
  };
}

describe('XtermView themes (issue #177 — OSC 11 probe)', () => {
  it('light theme background is opaque and bright', () => {
    // Codex CLI v0.132+ probes OSC 11 at startup; a transparent value
    // makes xterm.js report a useless fallback. Both branches must be
    // opaque so `color::is_light(bg)` returns a sensible answer.
    const { r, g, b, a } = parseHex(LIGHT_THEME.background!);
    expect(a).toBe(255);
    expect(r + g + b).toBeGreaterThan(200 * 3);
  });

  it('dark theme background is opaque and dark', () => {
    const { r, g, b, a } = parseHex(DARK_THEME.background!);
    expect(a).toBe(255);
    expect(r + g + b).toBeLessThan(128 * 3);
  });

  it('sends CSI focus-in to nudge codex re-probe on theme prop change', () => {
    // Codex caches the OSC 10/11 result from spawn and only re-probes on
    // `Event::FocusGained` (CSI \x1b[I). On a mid-session theme toggle
    // the theme-effect updates xterm.js's bg AND pushes \x1b[I to the
    // PTY so codex re-probes and picks up the new bg. The initial mount
    // must NOT send the focus-in — codex already got the right bg from
    // its spawn-time probe, and a stray focus event would be noise.
    const { rerender } = render(
      <XtermView terminalId="term_test" theme="dark" />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // Drop the ClientHello frame so the rest of the assertions only see
    // post-mount sends.
    ws.sentFrames.length = 0;

    // Re-render with the same theme — nothing should fire (the effect
    // skips when prev === current).
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    expect(ws.sentFrames).toHaveLength(0);

    // Toggle to light — expect a single Input frame carrying \x1b[I.
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="light" />);
    });
    expect(ws.sentFrames).toHaveLength(1);
    const frame = JSON.parse(ws.sentFrames[0]!);
    // \x1b[I = ESC [ I = [27, 91, 73]
    expect(frame).toEqual({ Input: { data: [27, 91, 73], input_seq: 0 } });
  });
});

describe('XtermView v2 resize wiring', () => {
  it('parses ResizeApplied without error', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    // Smoke test — the dispatch table needs to know how to ignore /
    // consume this without crashing.
    expect(() => {
      act(() => {
        ws.push({
          ResizeApplied: {
            epoch: 1,
            pty_seq: 11,
            render_rev: 6,
            cols: 120,
            rows: 30,
          },
        });
      });
    }).not.toThrow();
  });
});
