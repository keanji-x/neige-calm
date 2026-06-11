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
import type { EventMeta } from '../api/events';

// --- fake event stream -------------------------------------------------
//
// The fake mimics the EventStream public surface used by EventBridge:
// `subscribe(topics)` is a no-op (we don't care which topics it picks
// because we drive emits directly), and `on(fn)` registers a listener and
// returns an unsubscribe. `emit(ev)` is test-only and synchronously calls
// every listener — simulates the WS frame arrival path.
//
// The listener takes `(ev, meta)` since slice 5 (issue #56): meta carries
// the envelope `_id` / `eventVersion` so the trace ring buffer can stamp
// them. Tests that only care about the payload pass a synthetic `meta`
// — see `emit()` below for the default.
type Listener = (ev: WireEvent, meta: EventMeta) => void;
type ControlListener = () => void;
const fakeStream = {
  listeners: new Set<Listener>(),
  replayCompleteListeners: new Set<ControlListener>(),
  snapshotRequiredListeners: new Set<ControlListener>(),
  subscribe: vi.fn(),
  // Issue #198, concern 2: the bridge calls `setSyncEventVersion` before
  // `subscribe`. The fake records the value so a test can assert ordering
  // (see the "syncEventVersion is set before subscribe" case).
  setSyncEventVersion: vi.fn(),
  // Issue #198 followup: the bridge is the sole `start()` caller on the
  // shared singleton (`sharedEventStream()` no longer auto-starts). The
  // fake records the invocation so tests can assert it ran AFTER
  // setSyncEventVersion / subscribe — see the ordering case below.
  start: vi.fn(),
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
  emit(ev: WireEvent, meta: EventMeta = { id: 0, eventVersion: 1 }) {
    for (const fn of this.listeners) fn(ev, meta);
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
    this.setSyncEventVersion.mockClear();
    this.start.mockClear();
  },
};

vi.mock('../api/events', () => ({
  sharedEventStream: () => fakeStream,
}));

// Imported AFTER vi.mock so the module sees the mocked `sharedEventStream`.
import { EventBridge } from './eventBridge';

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

function seedWaveDetailWithCard(client: QueryClient, waveId: string, cardId: string) {
  client.setQueryData(['wave', waveId], {
    wave: {
      id: waveId,
      cove_id: 'cove_1',
      title: 'Wave',
      sort: 0,
      archived_at: null,
      pinned_at: null,
      lifecycle: 'draft',
      cwd: '',
      terminal_at: null,
      created_at: 1,
      updated_at: 2,
    },
    cards: [
      {
        id: cardId,
        wave_id: waveId,
        kind: 'terminal',
        sort: 0,
        payload: {},
        deletable: true,
        created_at: 1,
        updated_at: 2,
      },
    ],
    overlays: [],
  });
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
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    // dispatcher should have asked the stream for ['*'] — every event variant.
    expect(fakeStream.subscribe).toHaveBeenCalledWith(['*']);
    cleanup();
  });

  it('issue #198 — calls setSyncEventVersion BEFORE subscribe BEFORE start', () => {
    // The bridge must wire the server-declared sync event version into the
    // stream before any subscribe runs, so the very first frame the server
    // pushes (replay or live) is already gated by the eventVersion ceiling.
    // The followup (PR #215) also moves the singleton's `start()` call here
    // — `sharedEventStream()` no longer auto-starts — so the WS is genuinely
    // not opened until the bridge has finished wiring the gate. Ordering
    // must be: setSyncEventVersion → subscribe → start.
    const client = makeClient();
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={7} />
      </Wrapper>,
    );

    expect(fakeStream.setSyncEventVersion).toHaveBeenCalledTimes(1);
    expect(fakeStream.setSyncEventVersion).toHaveBeenCalledWith(7);
    expect(fakeStream.start).toHaveBeenCalledTimes(1);

    // Ordering: setSyncEventVersion → subscribe → start. We compare the
    // vi.fn invocation order via `mock.invocationCallOrder`.
    const setOrder = fakeStream.setSyncEventVersion.mock.invocationCallOrder[0];
    const subOrder = fakeStream.subscribe.mock.invocationCallOrder[0];
    const startOrder = fakeStream.start.mock.invocationCallOrder[0];
    expect(setOrder).toBeLessThan(subOrder);
    expect(subOrder).toBeLessThan(startOrder);
    cleanup();
  });

  it('cove.updated invalidates the coves list', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'cove.updated',
      data: {
        id: 'cove_1',
        name: 'Atlas',
        color: '#abc',
        sort: 0,
        kind: 'user',
        created_at: 1,
        updated_at: 2,
      },
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['coves'] });
    cleanup();
  });

  it('issue #288 — cove.updated writes the renamed payload through to the cache', () => {
    // Regression guard for the production sidebar-staleness bug.
    //
    // Before the fix, this arm only invalidated ['coves'] and relied on
    // a refetch round-trip to repaint the Sidebar. In production we
    // observed the refetch sometimes failing to surface to the Sidebar
    // even though `GET /api/coves` returned the new name. The fix is
    // a write-through: apply the event payload to the cache directly
    // so observers (Sidebar's `useCovesQuery`) see the new name on
    // the very next render, with no refetch dependency.
    //
    // This test seeds the cache with a stale name, fires `cove.updated`
    // with a renamed payload, and asserts the cache holds the new name
    // synchronously after dispatch — independent of any HTTP refetch.
    const client = makeClient();
    // Seed two coves so the test catches "I forgot to copy unaffected
    // rows" — a naive `setQueryData([{updated}])` would silently drop
    // the other cove.
    client.setQueryData(['coves'], [
      {
        id: 'cove_1',
        name: 'KeepMe',
        color: '#5a9',
        sort: 0,
        kind: 'user',
        created_at: 1,
        updated_at: 2,
      },
      {
        id: 'cove_2',
        name: 'OldName',
        color: '#c97',
        sort: 1,
        kind: 'user',
        created_at: 1,
        updated_at: 2,
      },
    ]);
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'cove.updated',
      data: {
        id: 'cove_2',
        name: 'NewName',
        color: '#c97',
        sort: 1,
        kind: 'user',
        created_at: 1,
        updated_at: 99,
      },
    });
    const cached = client.getQueryData<
      Array<{ id: string; name: string }>
    >(['coves']);
    expect(cached).toBeDefined();
    expect(cached!.find((c) => c.id === 'cove_1')?.name).toBe('KeepMe');
    expect(cached!.find((c) => c.id === 'cove_2')?.name).toBe('NewName');
    cleanup();
  });

  it('issue #288 — cove.updated is a no-op when the cove is not in cache', () => {
    // Defensive: a cove.updated event for a cove the client has never
    // fetched (or that was GC'd from the cache) must not crash and must
    // not synthesize a phantom row. The invalidate-on-the-side path
    // still triggers a refetch that lands the correct list on the next
    // mount of useCovesQuery.
    const client = makeClient();
    // No coves in cache.
    expect(client.getQueryData(['coves'])).toBeUndefined();
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'cove.updated',
      data: {
        id: 'cove_new',
        name: 'Phantom',
        color: '#abc',
        sort: 0,
        kind: 'user',
        created_at: 1,
        updated_at: 2,
      },
    });
    // Cache stays empty — we don't fabricate a row, we wait for the
    // refetch (driven by the sibling invalidate) to land the truth.
    expect(client.getQueryData(['coves'])).toBeUndefined();
    cleanup();
  });

  it('wave.updated invalidates both the cove list and the wave detail', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
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
        // Issue #145 — `lifecycle` is now part of the wave wire shape.
        lifecycle: 'draft',
        // Issue #250 PR 2 — cwd + terminal_at are part of the Wave
        // wire shape. The bridge doesn't care about either today;
        // future calendar/terminal-stamp subscribers will read them.
        cwd: '',
        terminal_at: null,
        pinned_at: null,
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

  it('card_added_with_terminal_id_invalidates_immediately', () => {
    // #13 contract: a single `card.added` carrying the final payload
    // (terminal_id stamped by the atomic-create endpoint) MUST invalidate
    // the owning wave detail synchronously — no debounce window, no
    // suppression. EventBridge calls `qc.invalidateQueries` directly in
    // the dispatch path; we don't need fake timers here.
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'card.added',
      data: {
        id: 'card_1',
        wave_id: 'wave_42',
        kind: 'terminal',
        sort: 0,
        payload: { terminal_id: 't_x', schemaVersion: 1 },
        // #229 PR A — user-deletable terminal card.
        deletable: true,
        created_at: 1,
        updated_at: 2,
      },
    });
    // The invalidate was synchronous — assert directly, no timer advance.
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_42'] });
    cleanup();
  });

  it('runtime.started invalidates owning wave detail, card overlays, and wave files', () => {
    const client = makeClient();
    seedWaveDetailWithCard(client, 'wave_1', 'card_runtime');
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'runtime.started',
      data: {
        runtime_id: 'runtime_1',
        card_id: 'card_runtime',
        kind: 'terminal',
        agent_provider: null,
        status: 'starting',
      },
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_1'] });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['overlays', 'card'] });
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: ['wave-files', 'wave_1'],
    });
    cleanup();
  });

  it('runtime.status_changed invalidates owning wave detail, card overlays, and wave files', () => {
    const client = makeClient();
    seedWaveDetailWithCard(client, 'wave_1', 'card_runtime');
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'runtime.status_changed',
      data: {
        runtime_id: 'runtime_1',
        card_id: 'card_runtime',
        old_status: 'starting',
        new_status: 'running',
      },
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_1'] });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['overlays', 'card'] });
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: ['wave-files', 'wave_1'],
    });
    cleanup();
  });

  it('runtime.superseded invalidates owning wave detail, card overlays, and wave files', () => {
    const client = makeClient();
    seedWaveDetailWithCard(client, 'wave_1', 'card_runtime');
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emit({
      ev: 'runtime.superseded',
      data: {
        old_runtime_id: 'runtime_1',
        new_runtime_id: 'runtime_2',
        card_id: 'card_runtime',
      },
    });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave', 'wave_1'] });
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['overlays', 'card'] });
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: ['wave-files', 'wave_1'],
    });
    cleanup();
  });

  it('plugin.state events are accepted but do not invalidate (no plugin query yet)', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
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
        <EventBridge syncEventVersion={1} />
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
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    fakeStream.emitSnapshotRequired();
    expect(clear).toHaveBeenCalledTimes(1);
    cleanup();
  });

  it('wave-fs projection events invalidate wave file queries', () => {
    // The report sidebar reads kernel-projected files. These events may
    // otherwise be consumed directly by card-topic listeners or the
    // dispatcher, but runs/* and cards/* hook views derive from them.
    const client = makeClient();
    seedWaveDetailWithCard(client, 'wave_1', 'card_worker');
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );

    const cases: Array<{ ev: WireEvent; queryKey: unknown[] }> = [
      {
        ev: {
          ev: 'codex.hook',
          data: {
            card_id: 'card_worker',
            kind: 'hook.codex.stop',
            hook_idempotency_key: 'hook-codex',
            payload: { transcript: 'done' },
          },
        },
        queryKey: ['wave-files', 'wave_1'],
      },
      {
        ev: {
          ev: 'claude.hook',
          data: {
            card_id: 'card_worker',
            kind: 'hook.claude.stop',
            hook_idempotency_key: 'hook-claude',
            payload: { transcript: 'done' },
          },
        },
        queryKey: ['wave-files', 'wave_1'],
      },
      {
        ev: {
          ev: 'codex.worker_requested',
          data: {
            idempotency_key: 'idem-codex',
            goal: 'g',
            context: null,
          },
        },
        queryKey: ['wave-files'],
      },
      {
        ev: {
          ev: 'terminal.worker_requested',
          data: {
            idempotency_key: 'idem-terminal',
            cmd: 'echo ok',
          },
        },
        queryKey: ['wave-files'],
      },
      {
        ev: {
          ev: 'task.completed',
          data: {
            idempotency_key: 'idem-done',
            result: null,
            artifacts: [],
          },
        },
        queryKey: ['wave-files'],
      },
      {
        ev: {
          ev: 'task.failed',
          data: {
            idempotency_key: 'idem-failed',
            reason: 'boom',
          },
        },
        queryKey: ['wave-files'],
      },
    ];

    for (const { ev, queryKey } of cases) {
      invalidate.mockClear();
      expect(() => fakeStream.emit(ev)).not.toThrow();
      expect(invalidate).toHaveBeenCalledWith({ queryKey });
    }

    cleanup();
  });

  it('hook events fall back to broad wave-file invalidation without cached ownership', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    expect(() =>
      fakeStream.emit({
        ev: 'codex.hook',
        data: {
          card_id: 'card_not_cached',
          kind: 'hook.codex.stop',
          hook_idempotency_key: 'hook-not-cached',
          payload: { transcript: 'done' },
        },
      }),
    ).not.toThrow();
    expect(invalidate).toHaveBeenCalledWith({ queryKey: ['wave-files'] });
    cleanup();
  });

  // Compile-time exhaustiveness evidence for PR #479 PR4:
  // Temporarily delete the `invalidationPolicies['wave.report_edited']` row
  // and run `npm run typecheck`. `definePolicies<T extends { [K in EventKind]:
  // InvalidationPolicy<K> }>` must make tsc reject the table with:
  // "Property 'wave.report_edited' is missing in type ... but required in type ..."
  it('wave.report_edited invalidates wave file queries', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    expect(() =>
      fakeStream.emit({
        ev: 'wave.report_edited',
        data: {
          wave_id: 'wave_1',
          card_id: 'card_report',
          author: 'spec',
          edit_id: 'edit_1',
          summary_before: 'before',
          summary_after: 'after',
          body_before: 'body before',
          body_after: 'body after',
        },
      }),
    ).not.toThrow();
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: ['wave-files', 'wave_1'],
    });
    cleanup();
  });

  it('harness.transcript.cleared dispatches as an explicit noop policy', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    expect(() =>
      fakeStream.emit({
        ev: 'harness.transcript.cleared',
        data: {
          runtime_id: 'runtime_2',
          card_id: 'card_spec',
          wave_id: 'wave_1',
        },
      }),
    ).not.toThrow();
    expect(invalidate).not.toHaveBeenCalled();
    cleanup();
  });

  it('harness.user_message.enqueued dispatches as an explicit noop policy', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    expect(() =>
      fakeStream.emit({
        ev: 'harness.user_message.enqueued',
        data: {
          runtime_id: 'runtime_2',
          card_id: 'card_spec',
          wave_id: 'wave_1',
          char_count: 9,
        },
      }),
    ).not.toThrow();
    expect(invalidate).not.toHaveBeenCalled();
    cleanup();
  });

  it('claude.hook without cached ownership invalidates all wave-file queries', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
      </Wrapper>,
    );
    expect(() =>
      fakeStream.emit({
        ev: 'claude.hook',
        data: {
          card_id: 'card_claude',
          kind: 'hook.claude.pre_tool_use',
          hook_idempotency_key: 'test-key',
          payload: { tool: 'Read' },
        },
      }),
    ).not.toThrow();
    expect(invalidate).toHaveBeenCalledWith({
      queryKey: ['wave-files'],
    });
    cleanup();
  });

  it('an event with an unmapped `ev` is ignored without throwing', () => {
    const client = makeClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const Wrapper = wrap(client);
    render(
      <Wrapper>
        <EventBridge syncEventVersion={1} />
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

  // ---- Issue #56 slice 5: event trace ring buffer ---------------------
  //
  // The bridge mirrors each WS frame into `window.__neigeEvents__` when
  // the dev build is loaded with `?trace=1`. Playwright reads that buffer
  // to make assertions about the event sequence that produced a UI state.
  // We test the gate (URL param + DEV flag) and the ring shape; the
  // helper-side surface lives under `web/e2e/helpers/trace.ts` and is
  // covered by the e2e smoke test.
  //
  // jsdom's `window.location` is read-only; we mutate it via
  // `window.history.replaceState` which jsdom honors, restoring the
  // empty search after each test so other specs in this file (which
  // don't expect the buffer to exist) aren't affected.
  describe('trace ring buffer', () => {
    function setTraceUrl(on: boolean): void {
      // `replaceState` updates `window.location.search` in-place under
      // jsdom — cleaner than redefining the `location` property and
      // doesn't leak across tests (we reset to `/` in afterEach).
      window.history.replaceState({}, '', on ? '/?trace=1' : '/');
    }
    function resetTraceGlobals(): void {
      delete window.__neigeEvents__;
      delete window.__neigeClearEvents__;
    }

    beforeEach(() => {
      resetTraceGlobals();
    });

    function renderBridge() {
      const client = makeClient();
      const Wrapper = wrap(client);
      render(
        <Wrapper>
          <EventBridge syncEventVersion={1} />
        </Wrapper>,
      );
      return client;
    }

    it('does not populate the buffer when `?trace=1` is absent', () => {
      setTraceUrl(false);
      renderBridge();
      fakeStream.emit({
        ev: 'cove.updated',
        data: {
          id: 'cove_x',
          name: 'X',
          color: '#fff',
          sort: 0,
          kind: 'user',
          created_at: 1,
          updated_at: 1,
        },
      });
      expect(window.__neigeEvents__).toBeUndefined();
      cleanup();
      setTraceUrl(false);
    });

    it('captures events into the ring buffer when `?trace=1` is set', () => {
      setTraceUrl(true);
      renderBridge();
      fakeStream.emit(
        {
          ev: 'cove.updated',
          data: {
            id: 'cove_a',
            name: 'A',
            color: '#aaa',
            sort: 0,
            kind: 'user',
            created_at: 1,
            updated_at: 1,
          },
        },
        { id: 17, eventVersion: 1 },
      );
      const buf = window.__neigeEvents__;
      expect(buf).toBeDefined();
      expect(buf!.length).toBe(1);
      expect(buf![0]).toMatchObject({
        id: 17,
        eventVersion: 1,
        ev: 'cove.updated',
      });
      expect(typeof buf![0].ts).toBe('number');
      cleanup();
      setTraceUrl(false);
    });

    it('caps the buffer at 200 entries and drops the oldest', () => {
      setTraceUrl(true);
      renderBridge();
      // Emit 205 events with monotonically increasing ids so we can
      // identify which were dropped. The shape doesn't matter as long
      // as it dispatches without throwing.
      for (let i = 1; i <= 205; i++) {
        fakeStream.emit(
          {
            ev: 'cove.updated',
            data: {
              id: `cove_${i}`,
              name: 'n',
              color: '#000',
              sort: 0,
              kind: 'user',
              created_at: 1,
              updated_at: 1,
            },
          },
          { id: i, eventVersion: 1 },
        );
      }
      const buf = window.__neigeEvents__!;
      expect(buf.length).toBe(200);
      // First entry should be id=6 (1..5 were ringed out), last id=205.
      expect(buf[0].id).toBe(6);
      expect(buf[buf.length - 1].id).toBe(205);
      cleanup();
      setTraceUrl(false);
    });

    it('exposes a clear function that empties the buffer in place', () => {
      setTraceUrl(true);
      renderBridge();
      fakeStream.emit({
        ev: 'cove.updated',
        data: {
          id: 'c',
          name: 'n',
          color: '#000',
          sort: 0,
          kind: 'user',
          created_at: 1,
          updated_at: 1,
        },
      });
      const before = window.__neigeEvents__;
      expect(before!.length).toBe(1);
      // Same reference must remain valid post-clear — the Playwright
      // helper holds onto the array reference across page.evaluate calls.
      const clearFn = window.__neigeClearEvents__;
      expect(typeof clearFn).toBe('function');
      clearFn!();
      expect(window.__neigeEvents__).toBe(before);
      expect(window.__neigeEvents__!.length).toBe(0);
      cleanup();
      setTraceUrl(false);
    });
  });
});
