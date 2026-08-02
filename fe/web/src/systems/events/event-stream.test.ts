import { describe, expect, it } from 'vitest';

import type { EventStreamDriver } from './event-stream.js';
import { EventStream } from './event-stream.js';

describe('EventStream behavior', () => {
  it('keeps the test escape hatch bridge-free but configuration-required', () => {
    const calls: string[] = [];
    const driver: EventStreamDriver = {
      start: (configuration) => calls.push(`start:${configuration.syncEventVersion}`),
      stop: () => calls.push('stop'),
    };
    const configured = EventStream.forTest('ws://test.invalid/api/events', driver).configure({
      syncEventVersion: 4,
      topics: ['card:c1'],
    });
    configured.start();
    configured.stop();
    expect(calls).toEqual(['start:4', 'stop']);
  });

  it('delivers the current disconnected state synchronously at registration', () => {
    const states: string[] = [];
    EventStream.forTest('ws://test.invalid/api/events', {
      start: () => undefined,
      stop: () => undefined,
    }).onConnectionState((state) => states.push(state));
    expect(states).toEqual(['disconnected']);
  });
});
