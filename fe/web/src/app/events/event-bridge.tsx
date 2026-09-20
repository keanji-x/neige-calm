// The React glue that owns exactly one configured event stream: mounted inside `ServerCompatGate`,
// the only `start()` caller in the tree; `configure()` is side-effect free.

import type { QueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';

import type { InvalidationContext } from '../../../../core/events/invalidation-plan.ts';
import { initialEventState, reduceEventFrame, type EventState } from '../../../../core/events/reducer.ts';
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';
import type { UnconfiguredEventStream } from '../../systems/events/event-stream.ts';
import { applyEventEffects } from './query-invalidation-adapter.ts';
import { trackLookupContext } from './track-lookup.ts';

export type EventBridgeProps = Readonly<{
  client: QueryClient;
  /** An unconfigured stream, created and owned by the caller — never a module singleton. */
  stream: UnconfiguredEventStream;
  syncEventVersion: number;
  dbInstanceId: string;
  cursor: SyncCursorPort;
  /** Lets card-scoped events resolve their owning track; defaults to "unknown". */
  context?: InvalidationContext;
}>;

/** Renders an inert marker so a contract test can assert where in the tree the bridge sits. */
export function EventBridge({ client, stream, syncEventVersion, dbInstanceId, cursor, context }: EventBridgeProps) {
  // The connection depends only on stream identity and the protocol ceiling; everything else is read
  // through a ref so fresh prop identities can never tear down and reopen the socket.
  const latest = useRef({ client, cursor, context, dbInstanceId });
  useEffect(() => {
    latest.current = { client, cursor, context, dbInstanceId };
  }, [client, cursor, context, dbInstanceId]);

  useEffect(() => {
    latest.current.cursor.adopt(latest.current.dbInstanceId);
    let state: EventState = initialEventState(syncEventVersion, latest.current.cursor.read());
    // configure() only freezes version/topics; nothing connects until start().
    const configured = stream.configure({ syncEventVersion, topics: ['*'] });
    const unsubscribe = stream.onFrame((frame) => {
      const current = latest.current;
      const reduction = reduceEventFrame(state, frame, current.context ?? trackLookupContext(current.client));
      state = reduction.state;
      for (const effect of reduction.effects) {
        if (effect.type === 'persist-cursor') current.cursor.write(effect.id);
        else if (effect.type === 'reconnect') { configured.stop(); configured.start(); }
        else applyEventEffects(current.client, [effect]);
      }
    });
    configured.start();
    return () => {
      unsubscribe();
      configured.stop();
    };
  }, [stream, syncEventVersion]);

  return <span data-nc-event-bridge="" hidden />;
}
