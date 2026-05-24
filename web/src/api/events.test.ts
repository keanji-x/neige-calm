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
// Issue #198, concern 2: future-protocol eventVersion gate.
//
// A frame stamped with `eventVersion > syncEventVersion` (the value the
// server declared on `/api/version`) is from a protocol the running
// frontend wasn't compiled for. The stream must DROP such a frame WITHOUT
// advancing the replay cursor — otherwise reconnecting under the same
// cursor would skip the frame forever, and a later, compatible frontend
// (e.g. after a hard refresh that loads a new bundle) would never see it.
//
// Contrast with the "malformed payload" path (zod parse failure for an
// in-range eventVersion): there we DO advance the cursor, because the
// frame's shape is wrong for *us* but its protocol version is one we
// claimed to understand.
// ---------------------------------------------------------------------------

describe('EventStream future-protocol eventVersion gate (issue #198)', () => {
  it('does not advance cursor on a frame whose eventVersion exceeds server-declared max', async () => {
    // Drain any pending `requestIdleCallback` fallbacks from prior tests
    // before snapshotting localStorage — otherwise a queued setTimeout from
    // a sibling test that advanced the cursor can fire mid-await and pollute
    // our null-assertion below. (Each test cleans up with `localStorage.
    // clear()` in beforeEach, but the queued flush runs after that.)
    await new Promise((r) => setTimeout(r, 0));
    localStorage.clear();

    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const s = new EventStream('ws://test/api/events');
      // Server says: "I emit eventVersion up to 1."
      s.setSyncEventVersion(1);
      s.subscribe(['*']);
      s.start();
      const ws = currentWs();
      ws.open();

      // A frame stamped with eventVersion=99 — from-the-future.
      ws.push({
        _id: 42,
        eventVersion: 99,
        ev: 'cove.deleted',
        data: { id: 'c-future' },
      });

      // Cursor MUST NOT advance.
      expect(s.cursor).toBeNull();
      // And localStorage should NOT have been written.
      await new Promise((r) => setTimeout(r, 0));
      expect(localStorage.getItem('calm:sync:cursor')).toBeNull();
      // We logged the drop so an operator can see it.
      expect(warn).toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it('still advances cursor on an in-range frame even if the payload is malformed', async () => {
    // Distinguishes from-the-future drops from "payload shape wrong for us"
    // drops. The latter should advance the cursor — otherwise a single bad
    // frame would pin the cursor and trigger an endless re-replay.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const s = new EventStream('ws://test/api/events');
      s.setSyncEventVersion(1);
      s.subscribe(['*']);
      s.start();
      const ws = currentWs();
      ws.open();

      // eventVersion=1 is in-range; the zod schema rejects on `ev: 'mystery'`.
      ws.push({
        _id: 5,
        eventVersion: 1,
        ev: 'mystery.thing',
        data: { foo: 'bar' },
      });

      // Cursor DID advance — this is not a future-protocol frame, so we
      // accept the server's "you saw this row" claim.
      expect(s.cursor).toBe(5);
      await new Promise((r) => setTimeout(r, 0));
      expect(localStorage.getItem('calm:sync:cursor')).toBe('5');
    } finally {
      warn.mockRestore();
    }
  });

  it('without setSyncEventVersion (bootstrap window) does not gate', () => {
    // Defensive path: if `setSyncEventVersion` was never called (the
    // EventBridge sets it before subscribe, but a test or pre-mount path
    // might not), the stream tolerates any eventVersion. This keeps
    // legacy / synthetic frames working.
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    ws.push({
      _id: 17,
      eventVersion: 99,
      ev: 'cove.deleted',
      data: { id: 'c-x' },
    });
    expect(s.cursor).toBe(17);
  });

  it('frames with equal eventVersion are accepted (boundary)', () => {
    const s = new EventStream('ws://test/api/events');
    s.setSyncEventVersion(2);
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    ws.push({
      _id: 3,
      eventVersion: 2,
      ev: 'cove.deleted',
      data: { id: 'c-z' },
    });
    expect(s.cursor).toBe(3);
  });

  it('exposes serverSyncEventVersion via the read-only getter', () => {
    const s = new EventStream('ws://test/api/events');
    expect(s.serverSyncEventVersion).toBeNull();
    s.setSyncEventVersion(5);
    expect(s.serverSyncEventVersion).toBe(5);
    // Idempotent / safe with same value.
    s.setSyncEventVersion(5);
    expect(s.serverSyncEventVersion).toBe(5);
    // Non-numeric / negative is ignored.
    s.setSyncEventVersion(-1);
    expect(s.serverSyncEventVersion).toBe(5);
    s.setSyncEventVersion(Number.NaN);
    expect(s.serverSyncEventVersion).toBe(5);
    // null clears.
    s.setSyncEventVersion(null);
    expect(s.serverSyncEventVersion).toBeNull();
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

  // -------------------------------------------------------------------------
  // Issue #290 — server-reset detection via `_replay_complete._id < cursor`.
  //
  // The server's `_replay_complete` carries the server's actual `events.id`
  // tip (`MAX(id)`) in `_id`. The dev `/dev/reset` path wipes
  // `sqlite_sequence`, so post-reset events restart at id=1 — a client whose
  // persisted cursor is from BEFORE the reset (e.g. id=42 from the prior
  // test) sees the new tip arrive much lower than its own cursor.
  //
  // The stream must treat that the same way as `_snapshot_required`: clear
  // the cursor, fire the snapshot listeners so the bridge `qc.clear()`s the
  // stale cache, and bounce the socket so the next reconnect comes up cold
  // under `since=0` and picks up the fresh log from the start. The regular
  // replay-complete listener should NOT fire on this path — the bridge's
  // defensive batch invalidate is the wrong response to a full reset (a
  // `qc.clear()` is what the cache needs).
  // -------------------------------------------------------------------------

  it('_replay_complete with _id < cursor triggers reset (clear cursor, fire snapshot listener)', async () => {
    // Drain any pending `requestIdleCallback` fallbacks from prior tests
    // before snapshotting localStorage — otherwise a queued setTimeout
    // from a sibling test that advanced the cursor can fire mid-await
    // and pollute our null-assertion below. (Each test cleans up with
    // `localStorage.clear()` in beforeEach, but the queued flush runs
    // after that.) Same pattern as the future-protocol gate suite.
    await new Promise((r) => setTimeout(r, 0));
    localStorage.clear();
    localStorage.setItem('calm:sync:cursor', '42');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    expect(s.cursor).toBe(42);

    const onSnapshot = vi.fn();
    s.onSnapshotRequired(onSnapshot);
    const onReplay = vi.fn();
    s.onReplayComplete(onReplay);

    // Suppress the diagnostic warn the stream emits on reset detection
    // so the test output stays clean.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      // Post-reset server tip is 7 — far below the client's stale cursor
      // of 42. This is the canary the issue pins.
      ws.push({ _id: 7, ev: '_replay_complete' });
    } finally {
      warn.mockRestore();
    }

    // The reset path mirrors `_snapshot_required`: cursor cleared,
    // snapshot listener fired, regular replay-complete listener NOT
    // fired (a `qc.invalidateQueries` is the wrong response to a full
    // log reset — `qc.clear()` is).
    expect(s.cursor).toBeNull();
    expect(onSnapshot).toHaveBeenCalledTimes(1);
    expect(onReplay).not.toHaveBeenCalled();
    // Flush the queued localStorage write so the assertion is stable.
    await new Promise((r) => setTimeout(r, 0));
    expect(localStorage.getItem('calm:sync:cursor')).toBeNull();
  });

  it('_replay_complete with _id == cursor does NOT trigger reset (normal zero-rows case)', () => {
    // Empty-replay-window happy path: server has no new events past the
    // client's cursor, so `_id` equals our `lastEventId`. Must NOT trip
    // the reset path — that would false-positive on every reconnect to
    // a quiet server.
    localStorage.setItem('calm:sync:cursor', '42');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const onSnapshot = vi.fn();
    s.onSnapshotRequired(onSnapshot);
    const onReplay = vi.fn();
    s.onReplayComplete(onReplay);

    ws.push({ _id: 42, ev: '_replay_complete' });

    // Cursor stays put; the regular replay-complete listener fires.
    expect(s.cursor).toBe(42);
    expect(onSnapshot).not.toHaveBeenCalled();
    expect(onReplay).toHaveBeenCalledTimes(1);
  });

  it('_replay_complete with _id > cursor advances cursor and does NOT trigger reset', () => {
    // Standard advance path. The frame brings news: server tip is past
    // our cursor, replay window was non-empty (or the server has rows we
    // didn't ask for in this scope). Cursor must advance, NO reset.
    localStorage.setItem('calm:sync:cursor', '42');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const onSnapshot = vi.fn();
    s.onSnapshotRequired(onSnapshot);

    ws.push({ _id: 99, ev: '_replay_complete' });

    expect(s.cursor).toBe(99);
    expect(onSnapshot).not.toHaveBeenCalled();
  });

  it('_replay_complete reset path bounces the socket so reconnect comes up cold', () => {
    // The reset path must close the WS so the auto-reconnect loop fires
    // a NEW connection. Without the bounce, the reset would clear the
    // cursor and fire the snapshot listener — but the same (now-stale)
    // socket would keep streaming live events the bridge already
    // cleared from cache, racing the re-bootstrap. Closing the socket
    // gives the next reconnect a `since=0` cold start that lines up
    // with the just-`qc.clear()`ed cache.
    localStorage.setItem('calm:sync:cursor', '42');
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      ws.push({ _id: 7, ev: '_replay_complete' });
    } finally {
      warn.mockRestore();
    }

    // The fake WS's `close()` sets `readyState = CLOSED` and fires the
    // close listener. Asserting on the readyState pins the bounce
    // behavior independent of how the auto-reconnect path is timed.
    expect(ws.readyState).toBe(WebSocket.CLOSED);
  });

  it('_replay_complete on a fresh client (null cursor) does NOT trigger reset', () => {
    // Cold-start: no persisted cursor yet. Even a `_id: 0` frame (empty
    // server log) must not trip the reset — there's no stale state to
    // clear. This pins the `this.lastEventId !== null` guard in the
    // reset check.
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const onSnapshot = vi.fn();
    s.onSnapshotRequired(onSnapshot);
    const onReplay = vi.fn();
    s.onReplayComplete(onReplay);

    ws.push({ _id: 0, ev: '_replay_complete' });

    // `_id: 0` doesn't advance the cursor (synthetic-emit sentinel
    // guard in `advanceCursor`), but the regular replay-complete path
    // still fires.
    expect(s.cursor).toBeNull();
    expect(onSnapshot).not.toHaveBeenCalled();
    expect(onReplay).toHaveBeenCalledTimes(1);
  });
});

// ---------------------------------------------------------------------------
// Connection-state observer (UI indicator for reconnect — issue follow-up)
// ---------------------------------------------------------------------------

describe('EventStream connection state', () => {
  it('starts disconnected and emits the current state synchronously on subscribe', () => {
    const s = new EventStream('ws://test/api/events');
    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    // Synchronous initial emission so React's useSyncExternalStore gets
    // a defined snapshot on first render.
    expect(states).toEqual(['disconnected']);
    expect(s.state).toBe('disconnected');
  });

  it('transitions disconnected → connecting on start()', () => {
    const s = new EventStream('ws://test/api/events');
    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    s.start();
    expect(states).toEqual(['disconnected', 'connecting']);
    expect(s.state).toBe('connecting');
  });

  it('does NOT transition to connected on WS open alone (replay window)', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();

    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    // After subscribe — current state captured by the sync emission.
    expect(states).toEqual(['connecting']);

    ws.open();
    // Just opening the socket is NOT "live" — server may still be
    // streaming replay frames. State must stay `connecting` until
    // `_replay_complete`.
    expect(s.state).toBe('connecting');
    expect(states).toEqual(['connecting']);
  });

  it('transitions to connected when _replay_complete arrives', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();

    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    expect(states).toEqual(['connecting']);

    ws.push({ _id: 1, ev: '_replay_complete' });
    expect(s.state).toBe('connected');
    expect(states).toEqual(['connecting', 'connected']);
  });

  it('transitions connected → connecting on socket close (reconnect path)', () => {
    vi.useFakeTimers();
    try {
      const s = new EventStream('ws://test/api/events');
      s.subscribe(['*']);
      s.start();
      const ws1 = currentWs();
      ws1.open();
      ws1.push({ _id: 1, ev: '_replay_complete' });
      expect(s.state).toBe('connected');

      const states: string[] = [];
      s.onConnectionState((st) => states.push(st));
      expect(states).toEqual(['connected']);

      // Socket dies; auto-reconnect schedules another connect() via
      // setTimeout(backoff). We must surface `connecting` IMMEDIATELY,
      // not just when the next socket opens.
      ws1.close();
      expect(s.state).toBe('connecting');
      expect(states).toEqual(['connected', 'connecting']);

      // Drive the reconnect; a new ws is constructed but state stays
      // `connecting` until the next `_replay_complete`.
      vi.advanceTimersByTime(1000);
      const ws2 = currentWs();
      expect(ws2).not.toBe(ws1);
      ws2.open();
      expect(s.state).toBe('connecting');

      ws2.push({ _id: 2, ev: '_replay_complete' });
      expect(s.state).toBe('connected');
      expect(states).toEqual(['connected', 'connecting', 'connected']);
    } finally {
      vi.useRealTimers();
    }
  });

  it('transitions to disconnected on explicit close()', () => {
    const s = new EventStream('ws://test/api/events');
    s.subscribe(['*']);
    s.start();
    const ws = currentWs();
    ws.open();
    ws.push({ _id: 1, ev: '_replay_complete' });

    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    expect(states).toEqual(['connected']);

    s.close();
    expect(s.state).toBe('disconnected');
    // After an explicit close, the WS close handler also fires —
    // but `this.closed` is true, so the reconnect branch is skipped
    // and no `connecting` transition is emitted.
    expect(states).toEqual(['connected', 'disconnected']);
  });

  it('unsubscribe stops further notifications', () => {
    const s = new EventStream('ws://test/api/events');
    const states: string[] = [];
    const off = s.onConnectionState((st) => states.push(st));
    expect(states).toEqual(['disconnected']);
    off();
    s.start();
    expect(states).toEqual(['disconnected']);
  });

  it('coalesces identical-state transitions (no duplicate emissions)', () => {
    const s = new EventStream('ws://test/api/events');
    const states: string[] = [];
    s.onConnectionState((st) => states.push(st));
    // After construct + sync emit.
    expect(states).toEqual(['disconnected']);

    s.start();
    expect(states).toEqual(['disconnected', 'connecting']);

    // A second `start()` is a no-op (the stream is already in `connecting`).
    s.start();
    expect(states).toEqual(['disconnected', 'connecting']);
  });
});

// ---------------------------------------------------------------------------
// Issue #198 followup: `sharedEventStream()` singleton lifecycle.
//
// PR #215 documented "EventBridge mounts after compat lands → WS opens"; in
// practice `ServerCompatGate` renders children eagerly while `q.data` is in
// flight, and any child that called `sharedEventStream()` (e.g. the
// connection-indicator hook, codex's hook listener) would trigger the
// singleton's auto-`start()` and open a socket BEFORE the compat verdict.
// The per-frame eventVersion gate's "tolerate null syncEventVersion"
// fallback kept this from being a correctness bug, but the documented
// invariant — "WS unattempted until compat verdict lands" — was overstated.
//
// The fix is to make the singleton inert until something explicitly calls
// `start()`. The EventBridge is the sole caller; sibling consumers only
// register observers. These tests pin the new contract.
// ---------------------------------------------------------------------------

describe('sharedEventStream singleton (issue #198 followup)', () => {
  let sharedEventStream: typeof import('./events').sharedEventStream;
  let _resetSharedStreamForTest: typeof import('./events')._resetSharedStreamForTest;

  beforeEach(async () => {
    // Re-import to grab the new symbols alongside `EventStream` (the
    // top-level `beforeEach` already calls `vi.resetModules`).
    ({ sharedEventStream, _resetSharedStreamForTest } = await import('./events'));
  });

  afterEach(() => {
    _resetSharedStreamForTest();
  });

  it('does NOT open a WebSocket on first access (in-flight race closed)', () => {
    // Simulate the race: `ServerCompatGate` renders children eagerly while
    // the version query is pending, and a child hook calls
    // `sharedEventStream()` to register an observer. The old code path
    // auto-`start()`ed the singleton here, which constructs a WebSocket
    // before `EventBridge` has had a chance to stamp the syncEventVersion
    // or even run at all. With the followup gate in place, no WS is
    // constructed until something — and in production, only
    // `EventBridge` — explicitly calls `start()`.
    const before = instances.length;
    const stream = sharedEventStream();
    expect(stream).toBeDefined();
    // The singleton was created but is inert: no FakeWebSocket constructor
    // call happened, and the state remains the "nothing's happening yet"
    // baseline. (Matches the disconnected→connecting→connected ladder's
    // initial rung documented in the ConnectionState comment.)
    expect(instances.length).toBe(before);
    expect(stream.state).toBe('disconnected');
  });

  it('returns the same instance on repeated access (singleton contract)', () => {
    const a = sharedEventStream();
    const b = sharedEventStream();
    expect(a).toBe(b);
    // Still no WS — repeated access doesn't change the "inert until
    // start()" invariant.
    expect(instances.length).toBe(0);
  });

  it('observer registration (on / onConnectionState / addTopic) works without start()', () => {
    // The whole point of the followup: callers that only OBSERVE (codex
    // listening for `codex.hook`, the connection indicator surfacing
    // `state`) must be able to call into the singleton during the in-
    // flight window without triggering a connect. Registration is
    // connection-agnostic by construction — these calls are safe no-ops
    // on the wire side.
    const stream = sharedEventStream();
    const off = stream.on(() => {});
    const offState = stream.onConnectionState(() => {});
    stream.addTopic('card:foo');
    // None of those triggered a socket.
    expect(instances.length).toBe(0);
    off();
    offState();
  });

  it('explicit start() opens a WebSocket (EventBridge path)', () => {
    // The bridge's contract: setSyncEventVersion → subscribe → start. We
    // exercise just the start hop here — the bridge wiring itself is
    // covered in `eventBridge.test.tsx`.
    const stream = sharedEventStream();
    stream.setSyncEventVersion(1);
    stream.subscribe(['*']);
    stream.start();
    // Now (and only now) the WS constructor fires.
    expect(instances.length).toBe(1);
    expect(stream.state).toBe('connecting');
  });
});
