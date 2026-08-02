import type { WireEvent } from '../../../../core/api/schemas.js';
import type { EventFrame, EventMeta, Topic } from '../../../../core/events/protocol.js';

export type ConnectionState = 'connecting' | 'connected' | 'disconnected';
export type EventHandler = (event: WireEvent, meta: EventMeta) => void;
export type ConnectionStateHandler = (state: ConnectionState) => void;
export type EventStreamConfiguration = Readonly<{
  syncEventVersion: number | null;
  topics: readonly Topic[];
}>;

export interface EventStreamSink {
  frame(frame: EventFrame): void;
  connectionState(state: ConnectionState): void;
}

export interface EventStreamDriver {
  start(configuration: EventStreamConfiguration, url: string, sink: EventStreamSink): void;
  stop(): void;
}

export interface UnconfiguredEventStream {
  on(handler: EventHandler): () => void;
  onConnectionState(handler: ConnectionStateHandler): () => void;
  configure(options: EventStreamConfiguration): ConfiguredEventStream;
}

export interface ConfiguredEventStream {
  start(): void;
  stop(): void;
}

function sameConfiguration(left: EventStreamConfiguration, right: EventStreamConfiguration): boolean {
  return left.syncEventVersion === right.syncEventVersion &&
    left.topics.length === right.topics.length &&
    left.topics.every((topic, index) => topic === right.topics[index]);
}

export class EventStream implements UnconfiguredEventStream {
  private readonly url: string;
  private readonly driver: EventStreamDriver;
  private readonly handlers = new Set<EventHandler>();
  private readonly stateHandlers = new Set<ConnectionStateHandler>();
  private configuration: EventStreamConfiguration | null = null;
  private configuredHandle: ConfiguredEventStream | null = null;
  private started = false;

  private constructor(url: string, driver: EventStreamDriver) {
    this.url = url;
    this.driver = driver;
  }

  static create(url: string, driver: EventStreamDriver): UnconfiguredEventStream {
    return new EventStream(url, driver);
  }

  on(handler: EventHandler): () => void {
    this.handlers.add(handler);
    return () => this.handlers.delete(handler);
  }

  onConnectionState(handler: ConnectionStateHandler): () => void {
    this.stateHandlers.add(handler);
    handler('disconnected');
    return () => this.stateHandlers.delete(handler);
  }

  configure(options: EventStreamConfiguration): ConfiguredEventStream {
    if (
      options.syncEventVersion !== null &&
      (!Number.isFinite(options.syncEventVersion) || options.syncEventVersion < 0)
    ) {
      throw new TypeError('syncEventVersion must be null or a finite non-negative number');
    }
    const frozen = Object.freeze({
      syncEventVersion: options.syncEventVersion,
      topics: Object.freeze([...options.topics]),
    });
    if (this.configuration !== null) {
      if (!sameConfiguration(this.configuration, frozen)) {
        throw new TypeError('EventStream is already configured with different options');
      }
      return this.configuredHandle as ConfiguredEventStream;
    }
    this.configuration = frozen;
    const sink: EventStreamSink = Object.freeze({
      frame: (frame: EventFrame) => {
        if (frame.type !== 'event') return;
        for (const handler of this.handlers) handler(frame.event, frame.meta);
      },
      connectionState: (state: ConnectionState) => {
        for (const handler of this.stateHandlers) handler(state);
      },
    });
    const handle: ConfiguredEventStream = Object.freeze({
      start: () => {
        if (this.started) return;
        this.started = true;
        this.driver.start(frozen, this.url, sink);
      },
      stop: () => this.driver.stop(),
    });
    this.configuredHandle = handle;
    return handle;
  }
}
