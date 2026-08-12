// @vitest-environment jsdom
import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort } from '../../../core/api/types.ts';
import { createFakeSocketFactory } from '../systems/events/fake-socket.ts';
import { WebSocketDriver } from '../systems/events/websocket-driver.ts';
import { createEventComposition } from './composition.ts';
import { EventBridge } from './events/event-bridge.tsx';

const transport: ApiTransportPort = { send: () => Promise.reject(new Error('unused')) };
function storage() {
  const values = new Map<string, string>();
  return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
}
function cardAdded(id: number, waveId = 'wave_1'): string {
  return JSON.stringify({
    ev: 'card.added', _id: id, eventVersion: 1,
    data: { id: 'card_1', wave_id: waveId, kind: 'terminal', sort: 5, payload: {}, created_at: 1000, updated_at: 2000 },
  });
}

afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); });

describe('event composition', () => {
  it('T-A1 shares one cursor store so reconnect subscribes from the synchronously delivered event id', () => {
    vi.useFakeTimers();
    const fake = createFakeSocketFactory();
    const driverFactory = vi.fn((options: ConstructorParameters<typeof WebSocketDriver>[0]) => new WebSocketDriver(options));
    const composition = createEventComposition({ storage: storage(), transport, socketFactory: fake.factory, driverFactory, url: 'ws://example/api/events' });
    render(<EventBridge client={new QueryClient()} stream={composition.stream} syncEventVersion={3} dbInstanceId="db-a" cursor={composition.store} />);
    expect(fake.constructionCount).toBe(1);
    fake.sockets[0].open(); fake.sockets[0].message(cardAdded(23));
    fake.sockets[0].close(); vi.advanceTimersByTime(0); vi.advanceTimersByTime(500);
    fake.sockets[1].open();
    expect(JSON.parse(fake.sockets[1].sent[0])).toEqual({ sub: ['*'], since: 23 });
    expect(driverFactory).toHaveBeenCalledOnce();
    expect(composition.driver).toBe(driverFactory.mock.results[0].value);
  });

  it('T-B2 carries a real socket card event through the bridge into wave invalidation', async () => {
    const fake = createFakeSocketFactory();
    const composition = createEventComposition({ storage: storage(), transport, socketFactory: fake.factory, url: 'ws://example/api/events' });
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, 'invalidateQueries').mockImplementation(() => Promise.resolve());
    render(<EventBridge client={client} stream={composition.stream} syncEventVersion={3} dbInstanceId="db-a" cursor={composition.store} />);
    await waitFor(() => expect(fake.constructionCount).toBe(1));
    fake.sockets[0].open(); fake.sockets[0].message(cardAdded(1, 'wave-live'));
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toContainEqual(['wave', 'wave-live']);
  });

  it('T-B3 snapshot-required clears once and bounces to exactly one since-zero socket', async () => {
    const fake = createFakeSocketFactory();
    const composition = createEventComposition({ storage: storage(), transport, socketFactory: fake.factory, url: 'ws://example/api/events' });
    const client = new QueryClient();
    const clear = vi.spyOn(client, 'clear');
    render(<EventBridge client={client} stream={composition.stream} syncEventVersion={3} dbInstanceId="db-a" cursor={composition.store} />);
    await waitFor(() => expect(fake.constructionCount).toBe(1));
    fake.sockets[0].open(); fake.sockets[0].message(cardAdded(8));
    fake.sockets[0].message(JSON.stringify({ ev: '_snapshot_required' }));
    expect(clear).toHaveBeenCalledOnce();
    expect(fake.constructionCount - fake.closeCount).toBe(1);
    fake.sockets[1].open();
    expect(JSON.parse(fake.sockets[1].sent[0])).toEqual({ sub: ['*'], since: 0 });
  });
});
