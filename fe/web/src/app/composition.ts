import { whoamiOperation } from '../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../core/api/types.ts';
import { createUnauthorizedChannel, type UnauthorizedChannel } from '../../../core/api/unauthorized.ts';
import { createBrowserCursorStore, type CursorIdleScheduler } from './events/browser-cursor-store.ts';
import { runOperation } from './providers/queries.ts';
import type { SyncCursorPort } from '../systems/events/cursor-port.ts';
import { EventStream, type UnconfiguredEventStream } from '../systems/events/event-stream.ts';
import { WebSocketDriver, wsUrl, type SocketFactory, type UnauthorizedProbe } from '../systems/events/websocket-driver.ts';

export type EventComposition = Readonly<{
  store: SyncCursorPort;
  driver: WebSocketDriver;
  stream: UnconfiguredEventStream;
}>;

export type EventDriverFactory = (options: ConstructorParameters<typeof WebSocketDriver>[0]) => WebSocketDriver;

export function createBrowserEventComposition(options: Readonly<{
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  transport: ApiTransportPort;
  unauthorizedChannel?: Pick<UnauthorizedChannel, 'notify'>;
}>): EventComposition {
  const unauthorized = options.unauthorizedChannel ?? createUnauthorizedChannel(
    { enqueue: (task) => queueMicrotask(task) },
    { report: console.error },
  );
  return createEventComposition({
    storage: options.storage,
    transport: options.transport,
    onUnauthorized: () => unauthorized.notify(),
  });
}

export function createEventComposition(options: Readonly<{
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  transport: ApiTransportPort;
  onUnauthorized?: () => void;
  socketFactory?: SocketFactory;
  idle?: CursorIdleScheduler;
  probeUnauthorized?: UnauthorizedProbe;
  driverFactory?: EventDriverFactory;
  url?: string;
}>): EventComposition {
  const store = createBrowserCursorStore(options.storage, options.idle);
  const createDriver = options.driverFactory ?? ((driverOptions) => new WebSocketDriver(driverOptions));
  const driver = createDriver({
    cursor: store,
    // The driver owns the single unauthorized notification for its probe.
    probeUnauthorized: options.probeUnauthorized ?? (() => runOperation(options.transport, whoamiOperation(), undefined)),
    onUnauthorized: options.onUnauthorized ?? (() => undefined),
    ...(options.socketFactory === undefined ? {} : { socketFactory: options.socketFactory }),
  });
  const stream = EventStream.create(options.url ?? wsUrl(), driver);
  return Object.freeze({ store, driver, stream });
}
