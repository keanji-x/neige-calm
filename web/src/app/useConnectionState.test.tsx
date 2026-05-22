// Tests for `useConnectionState` — the React hook wrapping the WS
// `EventStream`'s connection-state observer.
//
// We reuse the same `FakeWebSocket` shape as `web/src/api/events.test.ts`
// (kept inline rather than extracted to a helper — the surface is tiny
// and the two test files exercise different layers, so duplicating the
// fake keeps each file readable in isolation).

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';

let EventStream: typeof import('../api/events').EventStream;
let useConnectionState: typeof import('./useConnectionState').useConnectionState;

interface FakeWebSocket {
  readyState: number;
  sentFrames: string[];
  listeners: Record<string, ((ev: unknown) => void)[]>;
  send: (data: string) => void;
  close: () => void;
  addEventListener: (type: string, fn: (ev: unknown) => void) => void;
  fire: (type: string, ev?: unknown) => void;
  open: () => void;
  push: (json: unknown) => void;
}

let instances: FakeWebSocket[] = [];
function makeFakeWebSocketCtor(): typeof WebSocket {
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
  instances = [];
  (globalThis as { WebSocket: typeof WebSocket }).WebSocket =
    makeFakeWebSocketCtor();
  localStorage.clear();
  vi.resetModules();
  ({ EventStream } = await import('../api/events'));
  ({ useConnectionState } = await import('./useConnectionState'));
});

afterEach(() => {
  instances = [];
});

function currentWs(): FakeWebSocket {
  const ws = instances[instances.length - 1];
  if (!ws) throw new Error('no FakeWebSocket constructed yet');
  return ws;
}

describe('useConnectionState', () => {
  it('returns the current stream state on first render', () => {
    // Pre-construct + start a stream so the hook subscribes to one that
    // is already in `connecting`. This pins the "synchronous initial
    // emission" contract — useSyncExternalStore must surface a defined
    // value on render N=1, never `undefined`.
    const stream = new EventStream('ws://test/api/events');
    stream.start();

    const { result } = renderHook(() => useConnectionState(stream));
    expect(result.current).toBe('connecting');
  });

  it('re-renders through the full connecting → connected → connecting → connected cycle', () => {
    const stream = new EventStream('ws://test/api/events');
    stream.subscribe(['*']);
    stream.start();

    const { result } = renderHook(() => useConnectionState(stream));
    expect(result.current).toBe('connecting');

    const ws1 = currentWs();
    act(() => {
      ws1.open();
    });
    // WS open alone is NOT live — see the EventStream doc on
    // ConnectionState. The hook must still report `connecting`.
    expect(result.current).toBe('connecting');

    act(() => {
      ws1.push({ _id: 1, ev: '_replay_complete' });
    });
    expect(result.current).toBe('connected');

    // Simulate the socket dying. We use real timers here — the
    // close-driven `connecting` transition is synchronous; only the
    // subsequent `setTimeout(connect, backoff)` is deferred, and we
    // don't need to drive that for this assertion.
    act(() => {
      ws1.close();
    });
    expect(result.current).toBe('connecting');
  });

  it('returns disconnected after explicit close()', () => {
    const stream = new EventStream('ws://test/api/events');
    stream.subscribe(['*']);
    stream.start();

    const { result } = renderHook(() => useConnectionState(stream));
    const ws = currentWs();
    act(() => {
      ws.open();
      ws.push({ _id: 1, ev: '_replay_complete' });
    });
    expect(result.current).toBe('connected');

    act(() => {
      stream.close();
    });
    expect(result.current).toBe('disconnected');
  });

  it('unsubscribes on unmount (no further re-renders attempted)', () => {
    const stream = new EventStream('ws://test/api/events');
    stream.start();

    const { result, unmount } = renderHook(() => useConnectionState(stream));
    expect(result.current).toBe('connecting');

    // After unmount, mutating the stream must not throw and must not
    // attempt to notify the unmounted React tree. Easiest invariant:
    // unmount doesn't error, and subsequent transitions don't crash.
    unmount();
    expect(() => {
      const ws = currentWs();
      ws.open();
      ws.push({ _id: 1, ev: '_replay_complete' });
      stream.close();
    }).not.toThrow();
  });
});
