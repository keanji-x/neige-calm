// @vitest-environment jsdom
import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WireEvent } from '../../../../core/api/schemas.ts';
import type { EventFrame, Topic } from '../../../../core/events/protocol.ts';
import type {
  ConfiguredEventStream, EventFrameHandler, EventStreamConfiguration, UnconfiguredEventStream,
} from '../../systems/events/event-stream.ts';
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';
import { ServerCompatGate, WEB_COMPAT_VERSION, type ProviderRuntime } from '../providers/public.tsx';
import { EventBridge } from './event-bridge.tsx';

const compatible = {
  webCompatVersion: WEB_COMPAT_VERSION,
  minWebCompatVersion: WEB_COMPAT_VERSION,
  syncEventVersion: 3,
  dbInstanceId: 'db-a',
};

/**
 * Hand-written stream double. It mirrors the real typestate: handlers are only
 * reachable before `configure`, and only the configured handle can start.
 */
function fakeStream() {
  const record = {
    configureCalls: [] as EventStreamConfiguration[],
    startCalls: 0,
    stopCalls: 0,
    connected: false,
  };
  const handlers = new Set<EventFrameHandler>();
  const handle: ConfiguredEventStream = {
    start: () => { record.startCalls += 1; record.connected = true; },
    stop: () => { record.stopCalls += 1; record.connected = false; },
  };
  const stream: UnconfiguredEventStream = {
    on: () => () => undefined,
    onFrame: (handler) => { handlers.add(handler); return () => handlers.delete(handler); },
    onConnectionState: () => () => undefined,
    configure: (options) => {
      record.configureCalls.push(options);
      return handle;
    },
  };
  const emit = (frame: EventFrame) => { for (const handler of [...handlers]) handler(frame); };
  return { record, stream, emit, handlerCount: () => handlers.size };
}

function memoryCursor(initial: number | null = null): SyncCursorPort & { value: number | null } {
  return {
    value: initial,
    read() { return this.value; },
    write(cursor) { this.value = cursor; },
    adopt: () => undefined,
    clear() { this.value = null; },
  };
}

function runtime(overrides: Partial<ProviderRuntime> = {}): ProviderRuntime {
  const values = new Map<string, string>();
  return {
    fetchVersion: () => Promise.resolve(compatible),
    reload: vi.fn(),
    deleteDatabase: vi.fn(),
    idbDatabaseName: 'neige-calm',
    storage: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); },
      removeItem: (key: string) => { values.delete(key); },
    },
    ...overrides,
  };
}

/** Builds an event frame from a raw wire shape, including kinds core does not model. */
function eventFrame(id: number, event: { ev: string; data: unknown }): EventFrame {
  return { type: 'event', event: event as unknown as WireEvent, meta: { id, eventVersion: 1 } };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe('EventBridge contracts', () => {
  it('INV-APP-001 mounts inside ServerCompatGate and never exists before the compat verdict', async () => {
    const { record, stream } = fakeStream();
    const client = new QueryClient();
    let resolveVersion!: (value: typeof compatible) => void;
    const fetchVersion = () => new Promise<typeof compatible>((done) => { resolveVersion = done; });
    const view = render(
      <ServerCompatGate
        client={client}
        runtime={runtime({ fetchVersion })}
        cursorStore={{ clear: () => undefined }}
        renderEventBridge={(server) => (
          <EventBridge client={client} stream={stream} syncEventVersion={server.syncEventVersion} dbInstanceId={server.dbInstanceId} cursor={memoryCursor()} />
        )}
      >
        <b data-nc-route-child="">route</b>
      </ServerCompatGate>,
    );

    // Children render immediately; the bridge must not exist yet.
    expect(view.container.querySelector('[data-nc-route-child]')).not.toBeNull();
    expect(view.container.querySelector('[data-nc-event-bridge]')).toBeNull();
    expect(record.configureCalls).toHaveLength(0);
    expect(record.startCalls).toBe(0);

    resolveVersion(compatible);
    const marker = await waitFor(() => {
      const found = view.container.querySelector('[data-nc-event-bridge]');
      expect(found).not.toBeNull();
      return found as HTMLElement;
    });

    // Position, not mere presence: the marker is a descendant of the subtree the
    // gate rendered, alongside — never above — the gated children.
    const gateRoot = view.container.firstElementChild as HTMLElement;
    expect(gateRoot.parentElement).toBe(view.container);
    expect(view.container.contains(marker)).toBe(true);
    expect(marker.compareDocumentPosition(view.container.querySelector('[data-nc-route-child]') as HTMLElement))
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(record.configureCalls.map((call) => call.syncEventVersion)).toEqual([compatible.syncEventVersion]);
  });

  it('INV-APP-020 starts exactly once per stream instance across re-renders and remounts', async () => {
    const first = fakeStream();
    const client = new QueryClient();
    const cursor = memoryCursor();
    const view = render(
      <EventBridge client={client} stream={first.stream} syncEventVersion={3} dbInstanceId="db-a" cursor={cursor} />,
    );
    await waitFor(() => expect(first.record.startCalls).toBe(1));

    // Re-render with brand-new prop object identities: the connection must not
    // be torn down and reopened.
    for (let index = 0; index < 3; index += 1) {
      view.rerender(
        <EventBridge
          client={client}
          stream={first.stream}
          syncEventVersion={3}
          dbInstanceId={`db-${index}`}
          cursor={cursor}
          context={{ findWaveOwningCard: () => null }}
        />,
      );
    }
    expect(first.record.startCalls).toBe(1);
    expect(first.record.stopCalls).toBe(0);
    expect(first.handlerCount()).toBe(1);

    view.unmount();
    expect(first.record.stopCalls).toBe(1);
    expect(first.handlerCount()).toBe(0);

    // A second mount over its own stream starts that stream exactly once, and
    // never re-starts the first.
    const second = fakeStream();
    render(<EventBridge client={client} stream={second.stream} syncEventVersion={3} dbInstanceId="db-a" cursor={memoryCursor()} />);
    await waitFor(() => expect(second.record.startCalls).toBe(1));
    expect(first.record.startCalls).toBe(1);
  });

  it('INV-APP-021 configure does not connect; only start does', async () => {
    const record = { configured: 0, connected: 0 };
    const handle: ConfiguredEventStream = {
      start: () => { record.connected += 1; },
      stop: () => undefined,
    };
    const configureSeen: { topics: readonly Topic[] } [] = [];
    const stream: UnconfiguredEventStream = {
      on: () => () => undefined,
      onFrame: () => () => undefined,
      onConnectionState: () => () => undefined,
      configure: (options) => {
        record.configured += 1;
        configureSeen.push({ topics: options.topics });
        // The bridge must not have connected merely by configuring.
        expect(record.connected).toBe(0);
        return handle;
      },
    };
    render(<EventBridge client={new QueryClient()} stream={stream} syncEventVersion={3} dbInstanceId="db-a" cursor={memoryCursor()} />);
    await waitFor(() => expect(record.connected).toBe(1));
    expect(record.configured).toBe(1);
    expect(configureSeen).toEqual([{ topics: ['*'] }]);
  });

  it('ignores an unknown event kind without throwing and without touching the cache', async () => {
    const { record, stream, emit } = fakeStream();
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries');
    const clear = vi.spyOn(client, 'clear');
    render(<EventBridge client={client} stream={stream} syncEventVersion={3} dbInstanceId="db-a" cursor={memoryCursor()} />);
    await waitFor(() => expect(record.startCalls).toBe(1));

    expect(() => emit(eventFrame(1, { ev: 'totally.unknown', data: { nope: true } }))).not.toThrow();
    expect(clear).not.toHaveBeenCalled();
    expect(invalidate).not.toHaveBeenCalled();
  });

  it('clears the cache and reconnects on a snapshot-required frame', async () => {
    const { record, stream, emit } = fakeStream();
    const client = new QueryClient();
    const clear = vi.spyOn(client, 'clear');
    const cursor = memoryCursor(42);
    render(<EventBridge client={client} stream={stream} syncEventVersion={3} dbInstanceId="db-a" cursor={cursor} />);
    await waitFor(() => expect(record.startCalls).toBe(1));

    emit({ type: 'snapshot-required' });
    expect(clear).toHaveBeenCalledOnce();
    expect(cursor.value).toBeNull();
    expect(record.stopCalls).toBe(1);
    expect(record.startCalls).toBe(2);
    expect(record.connected).toBe(true);
  });

  it('persists the advancing cursor and invalidates the planned keys of a real event', async () => {
    const { record, stream, emit } = fakeStream();
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockImplementation(() => Promise.resolve());
    const cursor = memoryCursor();
    render(<EventBridge client={client} stream={stream} syncEventVersion={3} dbInstanceId="db-a" cursor={cursor} />);
    await waitFor(() => expect(record.startCalls).toBe(1));

    emit(eventFrame(9, { ev: 'wave.deleted', data: { id: 'w1', cove_id: 'c1' } }));
    expect(cursor.value).toBe(9);
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual([
      ['waves', 'c1'],
      ['overlays', 'wave'],
    ]);
  });

  it('resumes from the persisted cursor rather than replaying from zero', async () => {
    const { record, stream, emit } = fakeStream();
    const client = new QueryClient();
    const cursor = memoryCursor(20);
    render(<EventBridge client={client} stream={stream} syncEventVersion={3} dbInstanceId="db-a" cursor={cursor} />);
    await waitFor(() => expect(record.startCalls).toBe(1));

    emit(eventFrame(5, { ev: 'cove.deleted', data: { id: 'c1' } }));
    expect(cursor.value).toBe(20);
  });
});
