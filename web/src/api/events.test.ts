// Unit tests for the WS cursor / replay protocol on the client side
// (Scope D — sync engine phase 2). Mirrors the server-side contract in
// `crates/calm-server/src/ws/events.rs` and `tests/ws_replay.rs`.
//
// We don't actually open a TCP WebSocket; the tests swap in a tiny mock
// `WebSocket` constructor that the EventStream uses transparently. The
// mock lets each test push frames into the stream and inspect what the
// stream sends back (the {sub, since} payload, in particular).
//
// Why this lives outside the eventBridge test harness: the cursor
// protocol is owned by `EventStream` (api/events.ts). The bridge is
// just one consumer. Testing the stream in isolation here keeps the
// `EventStream` ↔ DOM-WebSocket boundary clear; the bridge integration
// gets its own coverage in `web/src/app/eventBridge.test.tsx`.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

// `loadCursor` reads from `localStorage`; we re-import the module after
// resetting state so each test starts from a clean cursor. (See `reset`
// helper below.)
let EventStream: typeof import('./events').EventStream;

interface FakeWebSocket {
  readyState: number;
  sentFrames: string[];
  listeners: Record<string, ((ev: unknown) => void)[]>;
  send: (data: string) => void;
  close: () => void;
  addEventListener: (type: string, fn: (ev: unknown) => void) => void;
  // Test-only helpers.
  fire: (type: string, ev?: unknown) => void;
  open: () => void;
  push: (json: unknown) => void;
}

/** Build a fresh `FakeWebSocket`. We capture the constructed instance via
 *  a module-scoped reference so tests can drive it. */
let instances: FakeWebSocket[] = [];
function makeFakeWebSocketCtor(): typeof WebSocket {
  // The real DOM `WebSocket` is a class with `OPEN` static, used by
  // `publishSub` to gate sends. We mirror that here.
  class FakeWS {
    static readonly CONNECTING = 0;
    static readonly OPEN = 1;
    static readonly CLOSING = 2;
    static readonly CLOSED = 3;
    readyState = FakeWS.CONNECTING;
    sentFrames: string[] = [];
    listeners: Record<string, ((ev: unknown) => void)[]> = {};
    url: string;
    constructor(url: string) {
      this.url = url;
      instances.push(this as unknown as FakeWebSocket);
    }
    send(data: string): void {
      this.sentFrames.push(data);
    }
    close(): void {
      this.readyState = FakeWS.CLOSED;
      this.fire('close');
    }
    addEventListener(type: string, fn: (ev: unknown) => void): void {
      (this.listeners[type] ||= []).push(fn);
    }
    // Test helpers.
    fire(type: string, ev: unknown = {}): void {
      (this.listeners[type] || []).forEach((fn) => fn(ev));
    }
    open(): void {
      this.readyState = FakeWS.OPEN;
      this.fire('open');
    }
    push(json: unknown): void {
      this.fire('message', { data: JSON.stringify(json) });
    }
  }
  return FakeWS as unknown as typeof WebSocket;
}

beforeEach(async () => {
  // Reset DOM globals between tests so each `EventStream` constructs
  // with a clean slate.
  instances = [];
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket = makeFakeWebSocketCtor();
  localStorage.clear();
  // Re-import so module-level state (the shared stream singleton's
  // closure-captured `requestIdleCallback` ref, mostly) is fresh.
  vi.resetModules();
  ({ EventStream } = await import('./events'));
});

afterEach(() => {
  instances = [];
});

function currentWs(): FakeWebSocket {
  const ws = instances[instances.length - 1];
  if (!ws) throw new Error('no FakeWebSocket constructed yet');
  return ws;
}

function lastSentSub(ws: FakeWebSocket): { sub: string[]; since?: number } {
  const frame = ws.sentFrames[ws.sentFrames.length - 1];
  if (!frame) throw new Error('no sub frame sent yet');
  return JSON.parse(frame);
}

// ---------------------------------------------------------------------------
// Cursor advancement + localStorage persistence
// ---------------------------------------------------------------------------

describe('EventStream cursor', () => {
  it('starts with no cursor when localStorage is empty', () => {
    const s = new EventStream('ws://test/api/events');
    expect(s.cursor).toBeNull();
  });

  it('hydrates cursor from localStorage on construct', () => {
    localStorage.setItem('calm:sync:cursor', '42');
    const s = new EventStream('ws://test/api/events');
    expect(s.cursor).toBe(42);
  });

  it('advances cursor on each valid frame and persists', async () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    ws.push({ _id: 7, ev: 'cove.deleted', data: { id: 'c-x' } });
    expect(s.cursor).toBe(7);
    // requestIdleCallback in jsdom falls back to setTimeout(0); flush.
    await new Promise((r) => setTimeout(r, 0));
    expect(localStorage.getItem('calm:sync:cursor')).toBe('7');

    // Larger id advances.
    ws.push({ _id: 9, ev: 'cove.deleted', data: { id: 'c-y' } });
    expect(s.cursor).toBe(9);
    await new Promise((r) => setTimeout(r, 0));
    expect(localStorage.getItem('calm:sync:cursor')).toBe('9');
  });

  it('ignores zero / missing / non-numeric _id (canary for unpersisted broadcasts)', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    // `_id: 0` is the synthetic-emit sentinel (bus.emit in tests). The
    // cursor must not regress to 0 — otherwise the next reconnect would
    // ask for a full replay it doesn't need.
    ws.push({ _id: 0, ev: 'cove.deleted', data: { id: 'c-x' } });
    expect(s.cursor).toBeNull();

    ws.push({ ev: 'cove.deleted', data: { id: 'c-x' } });
    expect(s.cursor).toBeNull();

    ws.push({ _id: 'nope', ev: 'cove.deleted', data: { id: 'c-x' } });
    expect(s.cursor).toBeNull();
  });

  it('never goes backwards (out-of-order safety)', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    ws.push({ _id: 100, ev: 'cove.deleted', data: { id: 'c-x' } });
    expect(s.cursor).toBe(100);

    // A smaller `_id` arriving after must NOT regress the cursor.
    ws.push({ _id: 50, ev: 'cove.deleted', data: { id: 'c-y' } });
    expect(s.cursor).toBe(100);
  });
});

// ---------------------------------------------------------------------------
// Reconnect → send `{sub, since}`
// ---------------------------------------------------------------------------

describe('EventStream reconnect', () => {
  it('sends since=<lastEventId> on first open when cursor exists', () => {
    localStorage.setItem('calm:sync:cursor', '17');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['wave:w-1']);
    s.start();
    const ws = currentWs();
    ws.open();

    const last = lastSentSub(ws);
    expect(last.sub).toEqual(['wave:w-1']);
    expect(last.since).toBe(17);
  });

  it('sends since=0 when no cursor is set (fresh client full replay)', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    // Fresh client must request `since: 0` so the server replays the full
    // event log — otherwise fixture-seeded events emitted before WS
    // connect never reach the client trace ring buffer.
    const last = lastSentSub(ws);
    expect(last.sub).toEqual(['*']);
    expect(last.since).toBe(0);
  });

  it('cursor persists across reconnect and is sent on the next sub', async () => {
    vi.useFakeTimers();
    try {
      const s = new EventStream('ws://test/api/events');
      s.subscribe(['*']);
      s.start();
      const ws1 = currentWs();
      ws1.open();

      ws1.push({ _id: 5, ev: 'cove.deleted', data: { id: 'c-x' } });
      // Flush requestIdleCallback fallback (setTimeout 0) under fake timers.
      vi.advanceTimersByTime(1);
      expect(localStorage.getItem('calm:sync:cursor')).toBe('5');

      // Connection drops; auto-reconnect uses `setTimeout(..., backoff)`
      // (starts at 500ms). Drive the timer forward to let it fire.
      ws1.close();
      vi.advanceTimersByTime(1000);

      const ws2 = currentWs();
      expect(ws2).not.toBe(ws1);
      ws2.open();

      const last = lastSentSub(ws2);
      expect(last.since).toBe(5);
    } finally {
      vi.useRealTimers();
    }
  });
});

// ---------------------------------------------------------------------------
// Control frames: `_replay_complete` and `_snapshot_required`
// ---------------------------------------------------------------------------

describe('EventStream control frames', () => {
  it('_replay_complete fires the replay listener and advances the cursor', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    let fired = 0;
    s.onReplayComplete(() => {
      fired += 1;
    });

    ws.push({ _id: 99, ev: '_replay_complete' });
    expect(fired).toBe(1);
    // The terminator stamps the cursor with the replay tip so the next
    // reconnect resumes correctly even when zero replay rows matched.
    expect(s.cursor).toBe(99);
  });

  it('_replay_complete is NOT dispatched to regular event listeners', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const onEvent = vi.fn();
    s.on(onEvent);
    ws.push({ _id: 99, ev: '_replay_complete' });
    expect(onEvent).not.toHaveBeenCalled();
  });

  it('_snapshot_required clears the cursor and fires the snapshot listener', async () => {
    localStorage.setItem('calm:sync:cursor', '100');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    expect(s.cursor).toBe(100);

    let fired = 0;
    s.onSnapshotRequired(() => {
      fired += 1;
    });

    ws.push({
      _id: 50000,
      ev: '_snapshot_required',
      data: { earliest_id: 50000 },
    });
    expect(fired).toBe(1);
    expect(s.cursor).toBeNull();
    expect(localStorage.getItem('calm:sync:cursor')).toBeNull();
  });

  it('_snapshot_required is NOT dispatched to regular event listeners', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const onEvent = vi.fn();
    s.on(onEvent);
    ws.push({
      _id: 50,
      ev: '_snapshot_required',
      data: { earliest_id: 50 },
    });
    expect(onEvent).not.toHaveBeenCalled();
  });
});
