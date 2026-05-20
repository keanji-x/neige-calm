// Unit tests for EventBridge — the WS → QueryClient invalidation translator.
//
// We don't open a real WebSocket. Instead, we replace `sharedEventStream`
// with a tiny in-memory fake that exposes the same `subscribe` + `on`
// surface plus an `emit` helper the test can use to fire synthetic events.
// EventBridge subscribes via `useEffect`, so we render it inside a real
// QueryClientProvider and spy on `queryClient.invalidateQueries` to assert
// the mapping in `dispatch()`.
//
// The 'event variants we care about (one assertion per dispatch arm):
//   - cove.updated      → invalidate ['coves']
//   - wave.updated      → invalidate ['waves', cove_id] AND ['wave', id]
//   - card.added        → invalidate ['wave', wave_id]
//   - plugin.state      → no invalidation (no plugin query yet)
//
// Unknown events shouldn't reach the dispatcher at all because the WS layer
// runtime-validates through zod before fanout — but we still verify that
// dispatch doesn't crash if a value with an unmapped `ev` slipped through.

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, cleanup } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { WireEvent } from '../api/wire';

// --- fake event stream -------------------------------------------------
//
// The fake mimics the EventStream public surface used by EventBridge:
// `subscribe(topics)` is a no-op (we don't care which topics it picks
// because we drive emits directly), and `on(fn)` registers a listener and
// returns an unsubscribe. `emit(ev)` is test-only and synchronously calls
// every listener — simulates the WS frame arrival path.
type Listener = (ev: WireEvent) => void;
type ControlListener = () => void;
const fakeStream = {
  listeners: new Set<Listener>(),
  replayCompleteListeners: new Set<ControlListener>(),
  snapshotRequiredListeners: new Set<ControlListener>(),
  subscribe: vi.fn(),
  on(fn: Listener) {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  },
  onReplayComplete(fn: ControlListener) {
    this.replayCompleteListeners.add(fn);
    return () => {
      this.replayCompleteListeners.delete(fn);
    };
  },
  onSnapshotRequired(fn: ControlListener) {
    this.snapshotRequiredListeners.add(fn);
    return () => {
      this.snapshotRequiredListeners.delete(fn);
    };
  },
  emit(ev: WireEvent) {
    for (const fn of this.listeners) fn(ev);
  },
  emitReplayComplete() {
    for (const fn of this.replayCompleteListeners) fn();
  },
  emitSnapshotRequired() {
    for (const fn of this.snapshotRequiredListeners) fn();
  },
  reset() {
    this.listeners.clear();
    this.replayCompleteListeners.clear();
    this.snapshotRequiredListeners.clear();
    this.subscribe.mockClear();
  },
};

vi.mock('../api/events', () => ({
  sharedEventStream: () => fakeStream,
}));

// Imported AFTER vi.mock so the module sees the mocked `sharedEventStream`.
import { EventBridge, suppressCardEvents } from './eventBridge';

// --- helpers ----------------------------------------------------------

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

function wrap(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

beforeEach(() => {
  fakeStream.reset();
});

describe('EventBridge', () => {
  it('subscribes to the wildcard topic on mount', () => {
    const client = makeClient();
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    // dispatcher should have asked the stream for ['*'] — every event variant.
    expect(fakeStream.subscribe).toHaveBeenCalledWith(['*']);
    cleanup();
  });

  it('cove.updated invalidates the coves list', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'cove.updated',
      data: {
        id: 'cove_1',
        name: 'Scratch',
        color: '#abc',
        sort: 0,
        created_at: 1,
        updated_at: 2,
      },
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['coves'] });
    cleanup();
  });

  it('wave.updated invalidates both the cove list and the wave detail', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'wave.updated',
      data: {
        id: 'wave_1',
        cove_id: 'cove_1',
        title: 'Hello',
        sort: 0,
        archived_at: null,
        created_at: 1,
        updated_at: 2,
      },
    });
    // Two invalidations, one per affected key. We don't care about ordering
    // — the bridge fires both, and TanStack Query coalesces refetches.
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['waves', 'cove_1'] });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_1'] });
    cleanup();
  });

  it('card.added invalidates the owning wave detail (debounced)', () => {
    vi.useFakeTimers();
    try {
      const client = makeClient();
      const invalidate = vi.spyOn(client, 'invalidateQueries');
      const Wrapper = wrap(client);
      render(
        <Wrapper>
          <EventBridge />
        </Wrapper>,
      );
      fakeStream.emit({
        ev: 'card.added',
        data: {
          id: 'card_1',
          wave_id: 'wave_42',
          kind: 'terminal',
          sort: 0,
          payload: { terminal_id: 't_x' },
          created_at: 1,
          updated_at: 2,
        },
      });
      // Card invalidations are debounced (~60ms) to coalesce rapid bursts
      // from multi-step kernel mutations. Nothing should fire yet.
      expect(invalidate).not.toHaveBeenCalledWith({ queryKey: ['wave', 'wave_42'] });
      vi.advanceTimersByTime(100);
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_42'] });
      cleanup();
    } finally {
      vi.useRealTimers();
    }
  });

  it('card.added + card.updated bursts coalesce into one invalidate', () => {
    vi.useFakeTimers();
    try {
      const client = makeClient();
      const invalidate = vi.spyOn(client, 'invalidateQueries');
      const Wrapper = wrap(client);
      render(
        <Wrapper>
          <EventBridge />
        </Wrapper>,
      );
      // Mirrors the terminal-card create flow: card.added (no terminal_id),
      // then card.updated (with terminal_id), within the debounce window.
      const baseCard = {
        id: 'card_1',
        wave_id: 'wave_42',
        kind: 'terminal',
        sort: 0,
        created_at: 1,
        updated_at: 2,
      } as const;
      fakeStream.emit({ ev: 'card.added', data: { ...baseCard, payload: null } });
      vi.advanceTimersByTime(15);
      fakeStream.emit({
        ev: 'card.updated',
        data: { ...baseCard, payload: { terminal_id: 't_x' } },
      });
      // Still pending — the second event reset the debounce window.
      const before = invalidate.mock.calls.filter((c) =>
        Array.isArray(c[0]?.queryKey)
          ? (c[0].queryKey as unknown[])[0] === 'wave'
          : false,
      ).length;
      expect(before).toBe(0);
      vi.advanceTimersByTime(100);
      const after = invalidate.mock.calls.filter((c) =>
        Array.isArray(c[0]?.queryKey)
          ? (c[0].queryKey as unknown[])[0] === 'wave'
          : false,
      ).length;
      // Exactly one ['wave', 'wave_42'] invalidation despite two events.
      expect(after).toBe(1);
      cleanup();
    } finally {
      vi.useRealTimers();
    }
  });

  it('suppressCardEvents skips invalidation for the marked wave, then resumes', () => {
    vi.useFakeTimers();
    try {
      const client = makeClient();
      const invalidate = vi.spyOn(client, 'invalidateQueries');
      const Wrapper = wrap(client);
      render(
        <Wrapper>
          <EventBridge />
        </Wrapper>,
      );

      const baseCard = {
        id: 'card_1',
        wave_id: 'wave_supp',
        kind: 'terminal',
        sort: 0,
        created_at: 1,
        updated_at: 2,
      } as const;

      // Mark the wave as self-mutating; the originating mutation will
      // fire its own invalidate when done. WS echoes for this wave must
      // be ignored entirely (not just debounced — they shouldn't refetch
      // at all while suppressed).
      const release = suppressCardEvents('wave_supp');
      fakeStream.emit({ ev: 'card.added', data: { ...baseCard, payload: null } });
      fakeStream.emit({
        ev: 'card.updated',
        data: { ...baseCard, payload: { terminal_id: 't_x' } },
      });
      vi.advanceTimersByTime(200);
      const waveInvalidations = invalidate.mock.calls.filter((c) =>
        Array.isArray(c[0]?.queryKey)
          ? (c[0].queryKey as unknown[])[0] === 'wave' &&
            (c[0].queryKey as unknown[])[1] === 'wave_supp'
          : false,
      ).length;
      expect(waveInvalidations).toBe(0);

      // After release, subsequent events fall through to the normal
      // debounced path.
      release();
      fakeStream.emit({
        ev: 'card.updated',
        data: { ...baseCard, payload: { terminal_id: 't_x' } },
      });
      vi.advanceTimersByTime(100);
      const after = invalidate.mock.calls.filter((c) =>
        Array.isArray(c[0]?.queryKey)
          ? (c[0].queryKey as unknown[])[0] === 'wave' &&
            (c[0].queryKey as unknown[])[1] === 'wave_supp'
          : false,
      ).length;
      expect(after).toBe(1);
      cleanup();
    } finally {
      vi.useRealTimers();
    }
  });

  it('suppressCardEvents refcount: nested suppress/release is balanced', () => {
    vi.useFakeTimers();
    try {
      const client = makeClient();
      const invalidate = vi.spyOn(client, 'invalidateQueries');
      const Wrapper = wrap(client);
      render(
        <Wrapper>
          <EventBridge />
        </Wrapper>,
      );

      const ev = (wave_id: string) =>
        ({
          ev: 'card.added',
          data: {
            id: 'c',
            wave_id,
            kind: 'terminal',
            sort: 0,
            payload: null,
            created_at: 0,
            updated_at: 0,
          },
        }) as const;

      const r1 = suppressCardEvents('wave_r');
      const r2 = suppressCardEvents('wave_r');
      // Release inner — outer still suppressing.
      r2();
      fakeStream.emit(ev('wave_r'));
      vi.advanceTimersByTime(100);
      expect(
        invalidate.mock.calls.some((c) =>
          Array.isArray(c[0]?.queryKey)
            ? (c[0].queryKey as unknown[])[1] === 'wave_r'
            : false,
        ),
      ).toBe(false);
      // Release outer — events go through.
      r1();
      fakeStream.emit(ev('wave_r'));
      vi.advanceTimersByTime(100);
      expect(
        invalidate.mock.calls.some((c) =>
          Array.isArray(c[0]?.queryKey)
            ? (c[0].queryKey as unknown[])[1] === 'wave_r'
            : false,
        ),
      ).toBe(true);
      cleanup();
    } finally {
      vi.useRealTimers();
    }
  });

  it('plugin.state events are accepted but do not invalidate (no plugin query yet)', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'plugin.state',
      data: { id: 'plug_1', state: 'Running' },
    });
    expect(invalidate).not.toHaveBeenCalled();
    cleanup();
  });

  // ---- Sync engine phase 2 (Scope D) control frames -------------------

  it('_replay_complete triggers a defensive batch invalidateQueries', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    // The bridge calls `qc.invalidateQueries()` with no arguments — that's
    // the broad-brush "every key" form, used to converge any optimistic
    // state drift across the replay window.
    fakeStream.emitReplayComplete();
    expect(invalidate).toHaveBeenCalled();
    // Confirm the call was the no-arg / no-key form (vs a targeted invalidate).
    const replayCall = invalidate.mock.calls.find(
      (c) => c.length === 0 || c[0] === undefined,
    );
    expect(replayCall).toBeTruthy();
    cleanup();
  });

  it('_snapshot_required clears the React Query cache', () => {
    const client = makeClient();
    const clear = vi.spyOn(client, 'clear');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    fakeStream.emitSnapshotRequired();
    expect(clear).toHaveBeenCalledTimes(1);
    cleanup();
  });

  it('an event with an unmapped `ev` is ignored without throwing', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge />
      </Wrapper>,
    );
    // Cast through unknown to bypass the discriminator — simulates a payload
    // that somehow leaked past the schema check (or a future variant the UI
    // hasn't been taught yet). The dispatcher's switch has no default, so we
    // assert it falls through without exploding.
    const unmapped = {
      ev: 'something.unknown',
      data: { foo: 'bar' },
    } as unknown as WireEvent;
    expect(() => fakeStream.emit(unmapped)).not.toThrow();
    expect(invalidate).not.toHaveBeenCalled();
    cleanup();
  });
});
