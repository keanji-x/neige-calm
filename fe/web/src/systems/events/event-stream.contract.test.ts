import { describe, expect, expectTypeOf, it } from 'vitest';

import type { WireEvent } from '../../../../core/api/schemas.js';
import type {
  ConfiguredEventStream,
  ConnectionState,
  EventStreamConfiguration,
  EventStreamDriver,
  EventStreamSink,
  UnconfiguredEventStream,
} from './event-stream.js';
import { EventStream } from './event-stream.js';

describe('event stream typestate contract', () => {
  it('[type-only] exposes handlers and configure only before configuration', () => {
    const compileOnly = false as boolean;
    if (compileOnly) {
      const stream = null as unknown as UnconfiguredEventStream;
      stream.on((_event: WireEvent) => void _event);
      stream.onFrame(() => undefined);
      stream.onConnectionState(() => undefined);
      // @ts-expect-error -- unconfigured streams cannot start; delete this whole line to verify the gate.
      void stream.start;

      const configured = stream.configure({ syncEventVersion: null, topics: ['*'] });
      configured.start();
      configured.stop();
      // @ts-expect-error -- handlers register before configure so the first frame cannot be missed.
      void configured.on;
      // @ts-expect-error -- configured handles cannot be configured again.
      void configured.configure;
      // @ts-expect-error -- frame handlers register before configure so control frames cannot be missed.
      void configured.onFrame;
    }
    expectTypeOf<UnconfiguredEventStream>().not.toHaveProperty('start');
    expectTypeOf<ConfiguredEventStream>().not.toHaveProperty('configure');
  });

  it('configure is lazy, repeat-equal is idempotent, and repeat-different is rejected', () => {
    const calls: string[] = [];
    const driver: EventStreamDriver = {
      start: () => calls.push('start'),
      stop: () => calls.push('stop'),
    };
    const stream = EventStream.create('ws://test.invalid/api/events', driver);
    const first = stream.configure({ syncEventVersion: 2, topics: ['*', 'wave:w1'] });
    expect(calls).toEqual([]);
    expect(stream.configure({ syncEventVersion: 2, topics: ['*', 'wave:w1'] })).toBe(first);
    expect(() => stream.configure({ syncEventVersion: 2, topics: ['*'] })).toThrow(TypeError);
    expect(() => stream.configure({ syncEventVersion: 2, topics: ['wave:w1', '*'] })).toThrow(TypeError);
    expect(() => stream.configure({ syncEventVersion: 3, topics: ['*'] })).toThrow(TypeError);
    first.start();
    expect(calls).toEqual(['start']);
  });

  it('rejects non-finite and negative protocol versions at the configuration boundary', () => {
    const driver: EventStreamDriver = { start: () => undefined, stop: () => undefined };
    for (const syncEventVersion of [-1, Number.NaN, Number.POSITIVE_INFINITY]) {
      const stream = EventStream.create('ws://test.invalid/api/events', driver);
      expect(() => stream.configure({ syncEventVersion, topics: ['*'] })).toThrow(TypeError);
    }
  });

  it('pins configuration and connection-state unions', () => {
    expectTypeOf<EventStreamConfiguration>().toEqualTypeOf<Readonly<{
      syncEventVersion: number | null;
      topics: readonly ('*' | `area:${string}` | `wave:${string}` | `card:${string}`)[];
    }>>();
    expectTypeOf<ConnectionState>().toEqualTypeOf<'connecting' | 'connected' | 'disconnected'>();
    expectTypeOf<EventStreamDriver['start']>().parameters.toEqualTypeOf<[
      EventStreamConfiguration,
      string,
      EventStreamSink,
    ]>();
  });
});
