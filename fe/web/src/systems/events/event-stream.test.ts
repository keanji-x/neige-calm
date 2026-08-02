import { describe, expect, it } from 'vitest';

import type { ConnectionState, EventStreamDriver, EventStreamSink } from './event-stream.js';
import { EventStream } from './event-stream.js';

describe('EventStream behavior', () => {
  it('starts and stops a configured driver without a separate test escape hatch', () => {
    const calls: string[] = [];
    const driver: EventStreamDriver = {
      start: (configuration) => calls.push(`start:${configuration.syncEventVersion}`),
      stop: () => calls.push('stop'),
    };
    const configured = EventStream.create('ws://test.invalid/api/events', driver).configure({
      syncEventVersion: 4,
      topics: ['card:c1'],
    });
    configured.start();
    configured.stop();
    expect(calls).toEqual(['start:4', 'stop']);
  });

  it('delivers the current disconnected state synchronously at registration', () => {
    const states: string[] = [];
    EventStream.create('ws://test.invalid/api/events', {
      start: () => undefined,
      stop: () => undefined,
    }).onConnectionState((state) => states.push(state));
    expect(states).toEqual(['disconnected']);
  });

  it('delivers an immediate first frame and driver connection states to pre-configure handlers', () => {
    const events: string[] = [];
    const states: string[] = [];
    const driver: EventStreamDriver = {
      start: (_configuration, _url, sink) => {
        const connecting: string = 'connecting';
        sink.connectionState(connecting as ConnectionState);
        sink.frame({
          type: 'event',
          event: { ev: 'cove.deleted', data: { id: 'c1' } },
          meta: { id: 1, eventVersion: 2 },
        });
        sink.connectionState('connected');
      },
      stop: () => undefined,
    };
    const stream = EventStream.create('ws://test.invalid/api/events', driver);
    stream.on((event, meta) => events.push(`${event.ev}:${meta.id}`));
    stream.onConnectionState((state) => states.push(state));

    stream.configure({ syncEventVersion: 2, topics: ['*'] }).start();

    expect(events).toEqual(['cove.deleted:1']);
    expect(states).toEqual(['disconnected', 'connecting', 'connected']);
  });

  it('misses an immediate first frame when a handler is registered after start', () => {
    let sink: EventStreamSink | null = null;
    const events: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => {
        sink = value;
        value.frame({
          type: 'event',
          event: { ev: 'cove.deleted', data: { id: 'c1' } },
          meta: { id: 1, eventVersion: 2 },
        });
      },
      stop: () => undefined,
    });
    stream.configure({ syncEventVersion: 2, topics: ['*'] }).start();
    stream.on((event) => events.push(event.ev));

    expect(sink).not.toBeNull();
    expect(events).toEqual([]);
  });
});
