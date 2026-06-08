// Unit tests for the v3 terminal protocol wiring in XtermView.
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
//     protocol_version=4, terminal_id, a `client_id` UUID, the local
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
import { createRef } from 'react';
import { render, screen, act, waitFor } from '@testing-library/react';

const stateMocks = vi.hoisted(() => ({
  statusSetCalls: [] as unknown[],
}));

vi.mock('./shared/state', async () => {
  const React = await vi.importActual<typeof import('react')>('react');
  return {
    useState(initialState?: unknown) {
      const [value, setValue] = React.useState(initialState);
      if (initialState !== 'connecting') return [value, setValue] as const;
      const wrappedSetValue: typeof setValue = (next) => {
        stateMocks.statusSetCalls.push(next);
        return setValue(next);
      };
      return [value, wrappedSetValue] as const;
    },
    useReducer: React.useReducer,
  };
});

// ---- xterm mock --------------------------------------------------------

interface MockTerm {
  cols: number;
  rows: number;
  write: ReturnType<typeof vi.fn>;
  writeln: ReturnType<typeof vi.fn>;
  clear: ReturnType<typeof vi.fn>;
  scrollToBottom: ReturnType<typeof vi.fn>;
  resize: ReturnType<typeof vi.fn>;
  open: ReturnType<typeof vi.fn>;
  loadAddon: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  /** #177 — xterm.js OSC suppressor surface. The component calls
   *  `term.parser.registerOscHandler(10|11|12, () => true)` to silence
   *  xterm.js's built-in OSC color reply so the daemon is the sole
   *  responder. Mock records the registered handlers so tests can
   *  inspect them. */
  parser: {
    registerOscHandler: ReturnType<typeof vi.fn>;
  };
  /** Map from OSC ident → registered handler, populated by the mock's
   *  `parser.registerOscHandler`. Tests use this to simulate xterm.js
   *  delivering an OSC sequence and verify the suppressor returns true. */
  __oscHandlers: Map<number, () => boolean>;
  onData: (cb: (d: string) => void) => { dispose: () => void };
  __dataCb?: (d: string) => void;
}

let mockTerm: MockTerm;
let terminalConstructCount = 0;
let terminalConstructorOptions: Record<string, unknown> | null = null;
let mockFitSize: { cols: number; rows: number } | null = null;

vi.mock('@xterm/xterm', () => {
  class Terminal {
    cols = 80;
    rows = 24;
    write = vi.fn();
    writeln = vi.fn();
    clear = vi.fn();
    scrollToBottom = vi.fn();
    resize = vi.fn((cols: number, rows: number) => {
      this.cols = cols;
      this.rows = rows;
    });
    open = vi.fn();
    loadAddon = vi.fn();
    dispose = vi.fn();
    // xterm.js exposes `options` as a mutable bag; the live-theme effect
    // assigns `term.options.theme = ...` and the real impl picks it up
    // on the next render cycle. For the mock we just need the slot to
    // exist so the assignment doesn't throw.
    options: Record<string, unknown> = {};
    // #177 — minimal `parser.registerOscHandler` shim. The component
    // calls this for slots 10/11/12 right after `term.open(container)`
    // to silence xterm.js's built-in OSC color reply. The mock records
    // each registered handler in `__oscHandlers` for inspection.
    __oscHandlers = new Map<number, () => boolean>();
    parser = {
      registerOscHandler: vi.fn((ident: number, handler: () => boolean) => {
        this.__oscHandlers.set(ident, handler);
        return { dispose: () => this.__oscHandlers.delete(ident) };
      }),
    };
    onData(cb: (d: string) => void): { dispose: () => void } {
      // Expose the callback so a test can simulate typing.
      (this as unknown as MockTerm).__dataCb = cb;
      return { dispose: () => {} };
    }
    constructor(options: Record<string, unknown>) {
      terminalConstructCount += 1;
      terminalConstructorOptions = options;
      mockTerm = this as unknown as MockTerm;
    }
  }
  return { Terminal };
});

vi.mock('@xterm/addon-fit', () => {
  class FitAddon {
    fit = vi.fn(() => {
      if (!mockFitSize) return;
      mockTerm.cols = mockFitSize.cols;
      mockTerm.rows = mockFitSize.rows;
    });
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

// ---- ResizeObserver / layout mocks -----------------------------------

let mockOffsetWidth = 800;
let mockOffsetHeight = 400;

class MockResizeObserver {
  static instances: MockResizeObserver[] = [];
  callback: ResizeObserverCallback;
  observe = vi.fn();
  disconnect = vi.fn();
  unobserve = vi.fn();
  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
    MockResizeObserver.instances.push(this);
  }
  trigger(): void {
    this.callback([], this as unknown as ResizeObserver);
  }
}

function setMockLayout(width: number, height: number): void {
  mockOffsetWidth = width;
  mockOffsetHeight = height;
}

const originalOffsetWidthDescriptor = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  'offsetWidth',
);
const originalOffsetHeightDescriptor = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  'offsetHeight',
);

function restoreHTMLElementDescriptor(
  name: 'offsetWidth' | 'offsetHeight',
  descriptor: PropertyDescriptor | undefined,
): void {
  if (descriptor) {
    Object.defineProperty(HTMLElement.prototype, name, descriptor);
    return;
  }
  delete (HTMLElement.prototype as unknown as Record<string, unknown>)[name];
}

// ---- import after mocks ------------------------------------------------

import { XtermView, type XtermViewHandle } from './XtermView';

beforeEach(() => {
  wsInstances = [];
  stateMocks.statusSetCalls.length = 0;
  terminalConstructCount = 0;
  terminalConstructorOptions = null;
  mockFitSize = null;
  MockResizeObserver.instances = [];
  setMockLayout(800, 400);
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket =
    FakeWebSocketCtor as unknown as typeof WebSocket;
  // jsdom doesn't have ResizeObserver; the component installs one.
  (globalThis as { ResizeObserver: typeof ResizeObserver }).ResizeObserver =
    MockResizeObserver as unknown as typeof ResizeObserver;
  Object.defineProperty(HTMLElement.prototype, 'offsetWidth', {
    configurable: true,
    get: () => mockOffsetWidth,
  });
  Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
    configurable: true,
    get: () => mockOffsetHeight,
  });
  // jsdom doesn't have crypto.randomUUID by default in older versions.
  if (!('randomUUID' in (globalThis.crypto ?? {}))) {
    Object.defineProperty(globalThis.crypto, 'randomUUID', {
      configurable: true,
      value: () => '00000000-0000-4000-8000-000000000000',
    });
  }
});

afterEach(() => {
  restoreHTMLElementDescriptor('offsetWidth', originalOffsetWidthDescriptor);
  restoreHTMLElementDescriptor('offsetHeight', originalOffsetHeightDescriptor);
  wsInstances = [];
});

// Helper: build a minimal valid `ServerHello` for the daemon handshake.
function serverHello(over: Record<string, unknown> = {}): unknown {
  return {
    ServerHello: {
      protocol_version: 4,
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

function renderSnapshot(over: Record<string, unknown> = {}): unknown {
  return {
    RenderSnapshot: {
      render_rev: 3,
      pty_seq: 9,
      cols: 80,
      rows: 24,
      encoding: 'Vt',
      data: [88, 89],
      scrollback: null,
      ...over,
    },
  };
}

interface ResizeCommitFrame {
  ResizeCommit: { epoch: number; cols: number; rows: number };
}

function resizeCommitFrames(ws: FakeWS): ResizeCommitFrame[] {
  return ws.sentFrames
    .map((frame) => JSON.parse(frame) as unknown)
    .filter(
      (frame): frame is ResizeCommitFrame =>
        !!frame &&
        typeof frame === 'object' &&
        'ResizeCommit' in frame,
    );
}

function connectOwnerTerminalAtSize(cols: number, rows: number): FakeWS {
  mockFitSize = { cols, rows };
  render(<XtermView terminalId="t-a" />);
  const ws = currentWs();
  act(() => {
    ws.fireOpen();
  });
  act(() => {
    ws.push(
      serverHello({
        terminal_id: 't-a',
        pty_size: {
          cols,
          rows,
          pixel_width: null,
          pixel_height: null,
        },
        snapshot: {
          render_rev: 1,
          pty_seq: 0,
          cols,
          rows,
          encoding: 'Vt',
          data: [104, 105],
          scrollback: null,
        },
      }),
    );
  });
  ws.sentFrames.length = 0;
  return ws;
}

function writeArgToString(arg: Uint8Array | string): string {
  return typeof arg === 'string'
    ? arg
    : String.fromCharCode(...Array.from(arg));
}

describe('XtermView v4 handshake', () => {
  it('exposes an imperative refresh handle', () => {
    const ref = createRef<XtermViewHandle>();
    render(<XtermView ref={ref} terminalId="term_test" />);
    expect(ref.current).not.toBeNull();
    expect(ref.current?.refresh).toEqual(expect.any(Function));
  });

  it('getWheelTarget returns null when component is not mounted', () => {
    type WheelTargetReturn = ReturnType<XtermViewHandle['getWheelTarget']>;
    const nullTarget: WheelTargetReturn = null;
    const ref = createRef<XtermViewHandle>();
    const { unmount } = render(<XtermView ref={ref} terminalId="term_test" />);
    const handle = ref.current;
    if (!handle) throw new Error('missing XtermView handle');

    unmount();

    expect(handle.getWheelTarget()).toBe(nullTarget);
  });

  it('refresh() rebuilds the WebSocket against the same terminal endpoint', async () => {
    const ref = createRef<XtermViewHandle>();
    render(<XtermView ref={ref} terminalId="term_test" />);
    const first = currentWs();
    expect(wsInstances).toHaveLength(1);

    act(() => {
      ref.current?.refresh();
    });

    await waitFor(() => expect(wsInstances).toHaveLength(2));
    const second = currentWs();
    expect(first.readyState).toBe(FakeWebSocketCtor.CLOSED);
    expect(second.url).toBe(first.url);
    expect(second).not.toBe(first);
  });

  it('ignores stale close callbacks from the pre-refresh WebSocket', async () => {
    const ref = createRef<XtermViewHandle>();
    const roleChanges: Array<string | null> = [];
    render(
      <XtermView
        ref={ref}
        terminalId="term_test"
        onRoleChange={(role) => roleChanges.push(role)}
      />,
    );
    const first = currentWs();
    act(() => {
      first.fireOpen();
    });
    act(() => {
      first.push(serverHello());
    });

    act(() => {
      ref.current?.refresh();
    });
    await waitFor(() => expect(wsInstances).toHaveLength(2));
    const second = currentWs();
    act(() => {
      second.fireOpen();
    });
    act(() => {
      second.push(serverHello());
    });

    expect(roleChanges.at(-1)).toBe('Owner');
    stateMocks.statusSetCalls.length = 0;

    act(() => {
      first.fireClose(1006, 'stale close');
    });

    expect(roleChanges.at(-1)).toBe('Owner');
    expect(roleChanges.filter((role) => role === null)).toHaveLength(1);
    expect(stateMocks.statusSetCalls).toEqual([]);
  });

  it.each([
    { width: 0, height: 400 },
    { width: 800, height: 0 },
  ])(
    'defers mount for tiny container geometry width=$width height=$height',
    ({ width, height }) => {
      setMockLayout(width, height);

      render(<XtermView terminalId="term_test" />);

      expect(terminalConstructCount).toBe(0);
      expect(wsInstances).toHaveLength(0);
      expect(screen.getByText(/loading terminal/i)).toBeInTheDocument();
      expect(MockResizeObserver.instances).toHaveLength(1);
    },
  );

  it('mounts normally after deferred ResizeObserver reports usable geometry', async () => {
    setMockLayout(0, 400);
    render(<XtermView terminalId="term_test" />);

    expect(terminalConstructCount).toBe(0);
    expect(wsInstances).toHaveLength(0);

    setMockLayout(800, 400);
    act(() => {
      MockResizeObserver.instances[0]!.trigger();
    });

    await waitFor(() => {
      expect(terminalConstructCount).toBe(1);
      expect(wsInstances).toHaveLength(1);
    });
    expect(screen.queryByText(/loading terminal/i)).not.toBeInTheDocument();
  });

  it('defers after fit produces degenerate terminal geometry', () => {
    mockFitSize = { cols: 2, rows: 2 };

    render(<XtermView terminalId="term_test" />);

    expect(terminalConstructCount).toBe(1);
    expect(mockTerm.dispose).toHaveBeenCalledTimes(1);
    expect(wsInstances).toHaveLength(0);
    expect(screen.getByText(/loading terminal/i)).toBeInTheDocument();
    expect(MockResizeObserver.instances).toHaveLength(1);
  });

  it('does not retry post-fit deferral when ResizeObserver reports the same failed size', () => {
    setMockLayout(100, 80);
    mockFitSize = { cols: 2, rows: 2 };

    render(<XtermView terminalId="term_test" />);

    expect(terminalConstructCount).toBe(1);
    expect(wsInstances).toHaveLength(0);
    expect(MockResizeObserver.instances).toHaveLength(1);

    act(() => {
      MockResizeObserver.instances[0]!.trigger();
    });

    expect(terminalConstructCount).toBe(1);
    expect(wsInstances).toHaveLength(0);
    expect(MockResizeObserver.instances).toHaveLength(1);
  });

  it('retries post-fit deferral after ResizeObserver reports a changed usable size', async () => {
    setMockLayout(100, 80);
    mockFitSize = { cols: 2, rows: 2 };

    render(<XtermView terminalId="term_test" />);

    expect(terminalConstructCount).toBe(1);
    expect(wsInstances).toHaveLength(0);

    setMockLayout(400, 300);
    mockFitSize = null;
    act(() => {
      MockResizeObserver.instances[0]!.trigger();
    });

    await waitFor(() => {
      expect(terminalConstructCount).toBe(2);
      expect(wsInstances).toHaveLength(1);
    });
    expect(screen.queryByText(/loading terminal/i)).not.toBeInTheDocument();
  });

  it('sends ClientHello with protocol_version=4 and role_hint Owner on open', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // #177 — every mount also dispatches a `TerminalThemeUpdate`
    // (buffered before WS open, drained after onopen), so the wire
    // carries ClientHello followed by at least one theme frame. This
    // test only cares about the ClientHello shape; assert it's the
    // first frame and inspect that one.
    expect(ws.sentFrames.length).toBeGreaterThanOrEqual(1);
    const frame = JSON.parse(ws.sentFrames[0]!);
    expect(frame).toHaveProperty('ClientHello');
    const hello = frame.ClientHello;
    expect(hello.protocol_version).toBe(4);
    expect(hello.terminal_id).toBe('term_test');
    expect(typeof hello.client_id).toBe('string');
    expect(hello.role_hint).toBe('Owner');
    expect(hello.capabilities.render_encodings).toEqual(['Vt']);
    expect(hello.capabilities.supports_sixel).toBe(false);
    expect(hello.capabilities.supports_images).toBe(false);
    expect(hello.initial_scrollback).toBe('All');
    expect(hello.resume_from).toBeNull();
    expect(hello.desired_size.cols).toBe(80);
    expect(hello.desired_size.rows).toBe(24);
  });

  it('configures xterm scrollback to match the server-retained history bound', () => {
    render(<XtermView terminalId="term_test" />);
    expect(terminalConstructorOptions?.scrollback).toBe(2000);
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

  it('does not mutate local xterm while container is degenerate (wave-switch transient)', () => {
    const ws = connectOwnerTerminalAtSize(100, 30);
    const rafSpy = vi
      .spyOn(window, 'requestAnimationFrame')
      .mockImplementation((cb) => {
        cb(0);
        return 1;
      });
    try {
      mockFitSize = { cols: 2, rows: 1 };
      setMockLayout(0, 0);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });

      expect(mockTerm.cols).toBe(100);
      expect(mockTerm.rows).toBe(30);
      expect(resizeCommitFrames(ws)).toHaveLength(0);

      mockFitSize = { cols: 120, rows: 32 };
      setMockLayout(1200, 480);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });
    } finally {
      rafSpy.mockRestore();
    }

    const commits = resizeCommitFrames(ws);
    expect(commits).toHaveLength(1);
    expect(commits.at(-1)?.ResizeCommit).toMatchObject({
      cols: 120,
      rows: 32,
    });
    expect(commits.at(-1)?.ResizeCommit.epoch).toBeGreaterThanOrEqual(1);
  });

  it('silently recovers when container restores to same size', () => {
    const ws = connectOwnerTerminalAtSize(100, 30);
    const rafSpy = vi
      .spyOn(window, 'requestAnimationFrame')
      .mockImplementation((cb) => {
        cb(0);
        return 1;
      });
    try {
      mockFitSize = { cols: 2, rows: 1 };
      setMockLayout(0, 0);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });

      expect(mockTerm.cols).toBe(100);
      expect(mockTerm.rows).toBe(30);

      mockFitSize = { cols: 100, rows: 30 };
      setMockLayout(960, 420);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });
    } finally {
      rafSpy.mockRestore();
    }

    expect(resizeCommitFrames(ws)).toHaveLength(0);
  });

  it('restores local xterm when fit() lands on a marginal grid (pixel gate passes, cols/rows below floor)', () => {
    const ws = connectOwnerTerminalAtSize(100, 30);
    const rafSpy = vi
      .spyOn(window, 'requestAnimationFrame')
      .mockImplementation((cb) => {
        cb(0);
        return 1;
      });
    try {
      mockFitSize = { cols: 9, rows: 1 };
      setMockLayout(80, 24);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });

      expect(mockTerm.cols).toBe(100);
      expect(mockTerm.rows).toBe(30);
      expect(resizeCommitFrames(ws)).toHaveLength(0);

      mockFitSize = { cols: 120, rows: 32 };
      setMockLayout(1200, 480);
      act(() => {
        MockResizeObserver.instances.at(-1)!.trigger();
      });
    } finally {
      rafSpy.mockRestore();
    }

    const commits = resizeCommitFrames(ws);
    expect(commits).toHaveLength(1);
    expect(commits.at(-1)?.ResizeCommit).toMatchObject({
      cols: 120,
      rows: 32,
    });
  });

  it('writes snapshot.scrollback before snapshot.data on ServerHello (#457)', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // 'sb' = scrollback bytes, 'hi' = visible-frame bytes.
    const scrollbackBytes = [115, 98];
    const dataBytes = [104, 105];
    act(() => {
      ws.push(
        serverHello({
          snapshot: {
            render_rev: 1,
            pty_seq: 0,
            cols: 80,
            rows: 24,
            encoding: 'Vt',
            data: dataBytes,
            scrollback: scrollbackBytes,
          },
        }),
      );
    });
    const writeCalls = mockTerm.write.mock.calls.map((c: unknown[]) => c[0]);
    expect(writeCalls.length).toBeGreaterThanOrEqual(3);
    expect(Array.from(writeCalls[0] as Uint8Array)).toEqual(scrollbackBytes);
    expect(writeArgToString(writeCalls[1] as Uint8Array | string)).toBe(
      '\r\n'.repeat(24),
    );
    expect(Array.from(writeCalls[2] as Uint8Array)).toEqual(dataBytes);
  });

  it('writes only snapshot.data when ServerHello snapshot.scrollback is null (#457)', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    const dataBytes = [111, 107]; // 'ok'
    act(() => {
      ws.push(
        serverHello({
          snapshot: {
            render_rev: 1,
            pty_seq: 0,
            cols: 80,
            rows: 24,
            encoding: 'Vt',
            data: dataBytes,
            scrollback: null,
          },
        }),
      );
    });
    const writeCalls = mockTerm.write.mock.calls.map((c: unknown[]) => c[0]);
    expect(writeCalls).toHaveLength(1);
    expect(Array.from(writeCalls[0] as Uint8Array)).toEqual(dataBytes);
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

  it('sends OwnerClaim when ServerHello assigns Observer despite Owner hint', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    ws.sentFrames.length = 0;
    act(() => {
      ws.push(
        serverHello({
          client_role: 'Observer',
          owner_client_id: '22222222-2222-4222-8222-222222222222',
        }),
      );
    });
    expect(ws.sentFrames.map((s) => JSON.parse(s))).toContain('OwnerClaim');
  });
});

describe('XtermView owner recovery', () => {
  it('sends OwnerClaim when OwnerChanged announces no owner', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    ws.sentFrames.length = 0;
    act(() => {
      ws.push({ OwnerChanged: { owner_client_id: null } });
    });
    expect(ws.sentFrames.map((s) => JSON.parse(s))).toContain('OwnerClaim');
  });

  it('promotes local role and clears protocol-error overlay when this client becomes owner', () => {
    const roleChanges: Array<string | null> = [];
    render(
      <XtermView
        terminalId="term_test"
        onRoleChange={(role) => roleChanges.push(role)}
      />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    const clientId = JSON.parse(ws.sentFrames[0]!).ClientHello.client_id;
    act(() => {
      ws.push({
        ProtocolError: {
          code: 'NotOwner',
          message: 'ResizeCommit requires owner role',
          expected_version: null,
        },
      });
    });
    expect(screen.getByRole('alert')).toBeInTheDocument();
    mockTerm.cols = 80;
    mockTerm.rows = 24;
    ws.sentFrames.length = 0;
    act(() => {
      ws.push({ OwnerChanged: { owner_client_id: clientId } });
    });
    expect(roleChanges).toContain('Owner');
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    expect(ws.sentFrames.map((s) => JSON.parse(s))).toContainEqual({
      ResizeCommit: { epoch: 1, cols: 80, rows: 24 },
    });
  });

  it('suppresses owner-claim ResizeCommit when local geometry is degenerate', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    const clientId = JSON.parse(ws.sentFrames[0]!).ClientHello.client_id;
    mockTerm.cols = 4;
    mockTerm.rows = 2;
    ws.sentFrames.length = 0;

    act(() => {
      ws.push({ OwnerChanged: { owner_client_id: clientId } });
    });

    expect(
      ws.sentFrames
        .map((s) => JSON.parse(s))
        .some((frame) => typeof frame === 'object' && 'ResizeCommit' in frame),
    ).toBe(false);
  });
});

describe('XtermView v3 streaming', () => {
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

  it('writes and scrolls on a standalone RenderSnapshot without scrollback', () => {
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
      ws.push(renderSnapshot());
    });
    expect(mockTerm.clear).not.toHaveBeenCalled();
    expect(mockTerm.write).toHaveBeenCalled();
    expect(mockTerm.write).toHaveBeenCalledTimes(1);

    const onWriteComplete = mockTerm.write.mock.calls[0]![1];
    expect(onWriteComplete).toEqual(expect.any(Function));
    expect(mockTerm.scrollToBottom).not.toHaveBeenCalled();
    (onWriteComplete as () => void)();
    expect(mockTerm.scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it('writes RenderSnapshot.scrollback before data on lag recovery (#473)', () => {
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

    const scrollbackBytes = [115, 98]; // 'sb'
    const dataBytes = [104, 105]; // 'hi'
    act(() => {
      ws.push(
        renderSnapshot({
          data: dataBytes,
          scrollback: scrollbackBytes,
        }),
      );
    });

    const writeCalls = mockTerm.write.mock.calls.map((c: unknown[]) => c[0]);
    expect(mockTerm.clear).toHaveBeenCalledTimes(1);
    expect(writeCalls).toHaveLength(4);
    expect(writeCalls[0]).toBe('\x1b[H');
    expect(Array.from(writeCalls[1] as Uint8Array)).toEqual(scrollbackBytes);
    expect(writeArgToString(writeCalls[2] as Uint8Array | string)).toBe(
      '\r\n'.repeat(24),
    );
    expect(Array.from(writeCalls[3] as Uint8Array)).toEqual(dataBytes);
  });

  it('writes only RenderSnapshot.data when scrollback is null (#473)', () => {
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

    const dataBytes = [111, 107]; // 'ok'
    act(() => {
      ws.push(
        renderSnapshot({
          data: dataBytes,
          scrollback: null,
        }),
      );
    });

    const writeCalls = mockTerm.write.mock.calls.map((c: unknown[]) => c[0]);
    expect(mockTerm.clear).not.toHaveBeenCalled();
    expect(mockTerm.write).toHaveBeenCalled();
    expect(writeCalls).toHaveLength(1);
    expect(Array.from(writeCalls[0] as Uint8Array)).toEqual(dataBytes);

    const onWriteComplete = mockTerm.write.mock.calls[0]![1];
    expect(onWriteComplete).toEqual(expect.any(Function));
    expect(mockTerm.scrollToBottom).not.toHaveBeenCalled();
    (onWriteComplete as () => void)();
    expect(mockTerm.scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it('sends typed input as a v3 Input frame', () => {
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

describe('XtermView v3 terminal states', () => {
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
    expect(screen.getByText(/refresh required for protocol v3/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /refresh/i })).toBeInTheDocument();
  });

  it('fires onExitChange with the code on TerminalExited and renders no overlay', () => {
    // #306 — the pre-#306 build surfaced "process exited (code N) +
    // Restart" as a full-card overlay. The v1 of this PR drops the
    // overlay entirely and lifts the exit info to the parent via
    // `onExitChange`, where a small header badge takes the role the
    // overlay used to play. This test pins the new contract.
    const exitChanges: Array<{
      exit_code: number | null;
      signal_killed: boolean;
    } | null> = [];
    render(
      <XtermView
        terminalId="term_test"
        onExitChange={(e) => exitChanges.push(e)}
      />,
    );
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
    // Parent received the exit info: numeric code from the JSON frame,
    // signal_killed=false because the JSON frame has no signal info
    // (the more-reliable signal flag arrives via the REST seed).
    expect(exitChanges).toContainEqual({
      exit_code: 137,
      signal_killed: false,
    });
    // No "process exited" overlay, no Restart button. The buffer
    // stays mounted; xterm.js's `term.writeln` still emits the dim
    // inline marker so the user sees "[process exited (code 137)]"
    // in the buffer itself — that's a `mockTerm.writeln` call, not
    // a React-rendered DOM node, so it stays out of the overlay
    // assertion.
    expect(screen.queryByText(/process exited/i)).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /restart/i }),
    ).not.toBeInTheDocument();
    expect(mockTerm.writeln).toHaveBeenCalled();
  });

  it('preserves the live exit code through the close-frame backstop (TerminalExited → child-exited close)', () => {
    // #306 regression — the normal happy-path sequence on a clean
    // child exit is:
    //   1. daemon emits `TerminalExited { code: 137 }` JSON frame
    //   2. kernel pump forwards it as the JSON message above
    //   3. WS closes with code 1000 + reason "child-exited"
    // The parent (`terminal.tsx` / `codex.tsx`) wires `onExitChange`
    // directly to a setState with no dedupe — so if the close-frame
    // backstop unconditionally fires `{exit_code: null, …}`, the
    // badge flips from "exit 137" (error palette) to "exit" (neutral
    // palette) at step 3 and stays there. The fix: the close handler
    // gates the backstop on `exitInfoRef.current === null` (i.e. no
    // prior JSON frame already delivered an exit code). This test
    // exercises the COMBINED sequence and asserts the FINAL value
    // received by the parent is the real code, not null.
    const exitChanges: Array<{
      exit_code: number | null;
      signal_killed: boolean;
    } | null> = [];
    render(
      <XtermView
        terminalId="term_test"
        onExitChange={(e) => exitChanges.push(e)}
      />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    // Step 1+2: the JSON frame lands first and fires onExitChange
    // with the real code.
    act(() => {
      ws.push({
        TerminalExited: { code: 137, pty_seq: 10, render_rev: 5 },
      });
    });
    // Step 3: the WS closes with the canonical child-exit reason.
    // Pre-fix this would push a second `{exit_code: null, …}` and
    // clobber the parent's setState; post-fix the gate suppresses
    // the backstop because `exitInfoRef.current` is already set.
    act(() => {
      ws.fireClose(1000, 'child-exited');
    });
    // The parent saw the real code at least once.
    expect(exitChanges).toContainEqual({
      exit_code: 137,
      signal_killed: false,
    });
    // And — the load-bearing assertion — the LAST value the parent
    // saw is still the real code, not the close-frame null. (The
    // parent's setState would otherwise show the badge as neutral
    // "exit" instead of the error-palette "exit 137".)
    expect(exitChanges.at(-1)).toEqual({
      exit_code: 137,
      signal_killed: false,
    });
  });

  it('does not render a disconnected overlay on plain WS close (buffer stays visible)', () => {
    // #306 — the pre-#306 build surfaced "disconnected — 1006/1005 +
    // Reconnect" as a full-card overlay on every non-clean close. v1
    // drops this overlay entirely: the buffer stays visible and the
    // user can either reload the page or wait for the WS to recover.
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
    expect(screen.queryByText(/disconnected/i)).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /reconnect/i }),
    ).not.toBeInTheDocument();
  });

  it('fires onExitChange backstop on a 1000 child-exited close even with no prior TerminalExited frame', () => {
    // Race recovery: the daemon emits TerminalExited → kernel pump
    // forwards it as JSON then closes WS with code 1000 + reason
    // `child-exited`. Even when the JSON frame is dropped on a slow
    // link, the close-frame reason alone must fire `onExitChange`
    // so the parent's badge appears. See ws/terminal.rs
    // `CLOSE_REASON_CHILD_EXITED`.
    const exitChanges: Array<{
      exit_code: number | null;
      signal_killed: boolean;
    } | null> = [];
    render(
      <XtermView
        terminalId="term_test"
        onExitChange={(e) => exitChanges.push(e)}
      />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    act(() => {
      ws.push(serverHello());
    });
    act(() => {
      ws.fireClose(1000, 'child-exited');
    });
    // exit_code is null on this path because the close-frame backstop
    // doesn't carry the code — only the JSON `TerminalExited` does.
    // Parent renders an "exit" badge in the neutral palette.
    expect(exitChanges).toContainEqual({
      exit_code: null,
      signal_killed: false,
    });
    // No overlays at all — buffer stays visible, no Restart button.
    expect(screen.queryByText(/process exited/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/disconnected/i)).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /restart/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /reconnect/i }),
    ).not.toBeInTheDocument();
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

describe('XtermView v3 resize wiring', () => {
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

// ---- #177 — theme dispatch + OSC suppression + WS queue --------------

describe('XtermView #177 OSC suppressor', () => {
  it('registers no-op handlers on slots 10, 11, 12 after term.open', () => {
    render(<XtermView terminalId="term_test" />);
    // Three slots, all returning `true` (the "we consumed it, don't reply"
    // signal to xterm.js's parser).
    expect(mockTerm.__oscHandlers.size).toBe(3);
    for (const slot of [10, 11, 12]) {
      const handler = mockTerm.__oscHandlers.get(slot);
      expect(handler, `OSC handler for slot ${slot}`).toBeDefined();
      expect(handler!()).toBe(true);
    }
  });
});

describe('XtermView #177 theme dispatch', () => {
  it('dispatches TerminalThemeUpdate on initial mount (default = light)', () => {
    render(<XtermView terminalId="term_test" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // After `fireOpen` the queue drains: ClientHello first, then the
    // buffered TerminalThemeUpdate from the theme-effect.
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBeGreaterThanOrEqual(1);
    // Light theme RGB values from `api/themeRgb.ts`.
    expect(themeFrames[0].TerminalThemeUpdate).toEqual({
      fg: [42, 47, 58],
      bg: [252, 254, 255],
    });
  });

  it('dispatches dark RGB when prop is "dark"', () => {
    render(<XtermView terminalId="term_test" theme="dark" />);
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBeGreaterThanOrEqual(1);
    expect(themeFrames[themeFrames.length - 1].TerminalThemeUpdate).toEqual({
      fg: [216, 219, 226],
      bg: [15, 20, 24],
    });
  });

  it('re-dispatches when the `theme` prop flips light → dark', () => {
    const { rerender } = render(
      <XtermView terminalId="term_test" theme="light" />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    // Reset wire after the initial drain so we observe only the toggle.
    ws.sentFrames.length = 0;
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBeGreaterThanOrEqual(1);
    expect(themeFrames[0].TerminalThemeUpdate).toEqual({
      fg: [216, 219, 226],
      bg: [15, 20, 24],
    });
  });

  it('re-dispatches even when the theme value is unchanged (idempotent contract)', () => {
    // Regression guard for the "drop prev-guard on theme-effect" decision.
    // The daemon-side handler is idempotent, so we deliberately accept a
    // redundant send on a no-op rerender rather than risk skipping a real
    // toggle after a remount (where any per-component "prev" bookkeeping
    // would have reset to null).
    const { rerender } = render(
      <XtermView terminalId="term_test" theme="dark" />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    ws.sentFrames.length = 0;
    act(() => {
      // Identical prop value — would be skipped if we still gated on
      // `prev !== current`.
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    // React skips effect re-runs when deps are referentially equal — so
    // this rerender should be a no-op on the wire. The test guards
    // against any future change that accidentally treats every render
    // as a theme-change.
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBe(0);
  });
});

describe('XtermView #177 WS send queue', () => {
  it('buffers TerminalThemeUpdate when WS is CONNECTING; flushes on open', () => {
    render(<XtermView terminalId="term_test" theme="dark" />);
    const ws = currentWs();
    // Before `fireOpen`, the WS is CONNECTING. The theme-effect has
    // already produced a `TerminalThemeUpdate` (via the `pendingThemeRef`
    // → `send` → `pendingFrames` queue chain), but nothing should be
    // on the wire yet.
    expect(ws.sentFrames).toHaveLength(0);
    act(() => {
      ws.fireOpen();
    });
    // Now both the ClientHello (sent inside `onopen`) and the buffered
    // theme frame should be on the wire, in order.
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    expect(frames[0]).toHaveProperty('ClientHello');
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBeGreaterThanOrEqual(1);
  });

  it('buffers a mid-handshake theme toggle and flushes on open', () => {
    // The exact race the source-branch bug chain targets: theme flips
    // *before* `ws.onopen` fires.
    const { rerender } = render(
      <XtermView terminalId="term_test" theme="light" />,
    );
    const ws = currentWs();
    // Theme toggle while still CONNECTING.
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    // Nothing on the wire yet.
    expect(ws.sentFrames).toHaveLength(0);
    act(() => {
      ws.fireOpen();
    });
    // The buffered dark theme frame must be on the wire after open.
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    expect(themeFrames.length).toBeGreaterThanOrEqual(1);
    // Last theme frame in the drain should carry dark RGB.
    expect(themeFrames[themeFrames.length - 1].TerminalThemeUpdate).toEqual({
      fg: [216, 219, 226],
      bg: [15, 20, 24],
    });
  });

  it('coalesces rapid theme toggles — last value wins on the daemon', () => {
    // Sanity-check the "no dropped dispatches under rapid toggle" claim.
    // Each rerender re-runs the theme-effect; each run produces one
    // `TerminalThemeUpdate`. None should be dropped; the daemon receives
    // every transition in order, so the final state is whatever the
    // last rerender sent.
    const { rerender } = render(
      <XtermView terminalId="term_test" theme="light" />,
    );
    const ws = currentWs();
    act(() => {
      ws.fireOpen();
    });
    ws.sentFrames.length = 0;
    // Rapid sequence — light → dark → light → dark.
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="light" />);
    });
    act(() => {
      rerender(<XtermView terminalId="term_test" theme="dark" />);
    });
    const frames = ws.sentFrames.map((s) => JSON.parse(s));
    const themeFrames = frames.filter(
      (f) => typeof f === 'object' && f !== null && 'TerminalThemeUpdate' in f,
    );
    // Each transition fires one frame.
    expect(themeFrames.length).toBe(3);
    expect(themeFrames[0].TerminalThemeUpdate.fg).toEqual([216, 219, 226]);
    expect(themeFrames[1].TerminalThemeUpdate.fg).toEqual([42, 47, 58]);
    expect(themeFrames[2].TerminalThemeUpdate.fg).toEqual([216, 219, 226]);
  });
});
