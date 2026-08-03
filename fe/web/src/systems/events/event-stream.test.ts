import { describe, expect, it } from 'vitest';

import type { EventStreamDriver, EventStreamSink } from './event-stream.js';
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
        sink.connectionState('connecting');
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

  it('delivers every frame variant through the public frame consumer', () => {
    let sink: EventStreamSink | undefined;
    const seen: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => { sink = value; },
      stop: () => undefined,
    });
    stream.onFrame((frame) => seen.push(frame.type));
    stream.configure({ syncEventVersion: 2, topics: ['*'] }).start();

    sink?.frame({ type: 'replay-complete', id: 4 });
    sink?.frame({ type: 'snapshot-required' });
    sink?.frame({
      type: 'malformed-event',
      id: 5,
      eventVersion: 2,
      error: { kind: 'decode', message: 'bad event', cause: null },
    });
    sink?.frame({
      type: 'event',
      event: { ev: 'cove.deleted', data: { id: 'c1' } },
      meta: { id: 6, eventVersion: 2 },
    });

    expect(seen).toEqual(['replay-complete', 'snapshot-required', 'malformed-event', 'event']);
  });

  it('stops delivering frames after the frame handler unsubscribes', () => {
    let sink: EventStreamSink | undefined;
    const seen: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => { sink = value; },
      stop: () => undefined,
    });
    const off = stream.onFrame((frame) => seen.push(frame.type));
    stream.configure({ syncEventVersion: 2, topics: ['*'] }).start();
    off();
    sink?.frame({ type: 'snapshot-required' });
    expect(seen).toEqual([]);
  });

  it('does not deliver control or malformed frames to event handlers', () => {
    let sink: EventStreamSink | undefined;
    const events: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => { sink = value; },
      stop: () => undefined,
    });
    stream.on((event) => events.push(event.ev));
    stream.configure({ syncEventVersion: 2, topics: ['*'] }).start();
    sink?.frame({ type: 'replay-complete', id: 4 });
    sink?.frame({ type: 'snapshot-required' });
    sink?.frame({
      type: 'malformed-event', id: 5, eventVersion: 2,
      error: { kind: 'decode', message: 'bad event', cause: null },
    });
    expect(events).toEqual([]);
  });

  it('keeps consecutive starts idempotent', () => {
    let startCount = 0;
    const configured = EventStream.create('ws://test.invalid/api/events', {
      start: () => { startCount += 1; },
      stop: () => undefined,
    }).configure({ syncEventVersion: 2, topics: ['*'] });

    configured.start();
    configured.start();

    expect(startCount).toBe(1);
  });

  it('can start again after it is stopped', () => {
    let startCount = 0;
    const configured = EventStream.create('ws://test.invalid/api/events', {
      start: () => { startCount += 1; },
      stop: () => undefined,
    }).configure({ syncEventVersion: 2, topics: ['*'] });

    configured.start();
    configured.stop();
    configured.start();

    expect(startCount).toBe(2);
  });

  it('cleans up a failed driver start before retrying', () => {
    let startCount = 0;
    let stopCount = 0;
    let activeResources = 0;
    const configured = EventStream.create('ws://test.invalid/api/events', {
      start: () => {
        startCount += 1;
        activeResources += 1;
        if (startCount === 1) throw new Error('start failed');
      },
      stop: () => {
        stopCount += 1;
        activeResources -= 1;
      },
    }).configure({ syncEventVersion: 2, topics: ['*'] });

    expect(() => configured.start()).toThrow('start failed');
    expect(stopCount).toBe(1);

    configured.start();

    expect(startCount).toBe(2);
    expect(activeResources).toBe(1);
  });

  it('broadcasts disconnected on stop while rejecting the driver stop callback', () => {
    const states: string[] = [];
    let sink: EventStreamSink | undefined;
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => {
        sink = value;
        value.connectionState('connected');
      },
      stop: () => sink?.connectionState('disconnected'),
    });
    stream.onConnectionState((state) => states.push(state));
    const configured = stream.configure({ syncEventVersion: 2, topics: ['*'] });
    configured.start();
    configured.stop();
    expect(states).toEqual(['disconnected', 'connected', 'disconnected']);
  });

  it('rejects callbacks from an old generation after restart', () => {
    const sinks: EventStreamSink[] = [];
    const seen: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, sink) => { sinks.push(sink); },
      stop: () => undefined,
    });
    stream.onFrame((frame) => seen.push(frame.type));
    const configured = stream.configure({ syncEventVersion: 2, topics: ['*'] });
    configured.start();
    configured.stop();
    configured.start();
    sinks[0]?.frame({ type: 'snapshot-required' });
    sinks[1]?.frame({ type: 'replay-complete', id: 1 });
    expect(seen).toEqual(['replay-complete']);
  });

  it('ignores driver delivery after stop', () => {
    let sink: EventStreamSink | undefined;
    const seen: string[] = [];
    const stream = EventStream.create('ws://test.invalid/api/events', {
      start: (_configuration, _url, value) => { sink = value; },
      stop: () => undefined,
    });
    stream.onFrame((frame) => seen.push(frame.type));
    stream.onConnectionState((state) => seen.push(state));
    const configured = stream.configure({ syncEventVersion: 2, topics: ['*'] });
    configured.start();
    configured.stop();
    seen.length = 0;

    sink?.frame({ type: 'snapshot-required' });
    sink?.connectionState('connected');

    expect(seen).toEqual([]);
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
