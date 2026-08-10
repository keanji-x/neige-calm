// The React glue that owns exactly one configured event stream.
//
// INV-APP-001 — mounted *inside* `ServerCompatGate` via its `renderEventBridge`
// prop, so no stream can exist before the compat verdict lands.
// INV-APP-020 — the only `start()` caller in the tree. Other consumers may
// register handlers on the unconfigured stream; they must not connect.
// INV-APP-021 — `configure()` is side-effect free; connecting happens in the
// effect, on `start()`.
// GATE-APP-079 — this slice ships no dev trace buffer at all, so there is no
// `import.meta.env.DEV` short-circuit to write and nothing for the production
// bundle to carry. Any future tracing must be inlined at the call site below.

import type { QueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';

import type { InvalidationContext } from '../../../../core/events/invalidation-plan.ts';
import { initialEventState, reduceEventFrame, type EventState } from '../../../../core/events/reducer.ts';
import type { UnconfiguredEventStream } from '../../systems/events/event-stream.ts';
import { applyEventEffects } from './query-invalidation-adapter.ts';

/**
 * Cursor persistence port. The implementation must store under
 * `SYNC_CURSOR_KEY` from `core/keys/storage.ts`; the bridge never touches
 * `localStorage` itself, which is why the port is injected rather than built
 * here.
 */
export interface SyncCursorPort {
  read(): number | null;
  write(cursor: number | null): void;
}

export type EventBridgeProps = Readonly<{
  client: QueryClient;
  /** An unconfigured stream, created and owned by the caller — never a module singleton. */
  stream: UnconfiguredEventStream;
  syncEventVersion: number;
  cursor: SyncCursorPort;
  /** Lets card-scoped events resolve their owning wave; defaults to "unknown". */
  context?: InvalidationContext;
}>;

/**
 * Renders an inert marker so a contract test can assert *where* in the tree the
 * bridge sits, not merely that it ran.
 */
export function EventBridge({ client, stream, syncEventVersion, cursor, context }: EventBridgeProps) {
  // The connection depends only on stream identity and the negotiated protocol
  // ceiling. Cache client, cursor store and lookup context are read through a
  // ref so a re-render with fresh prop identities can never tear down and
  // reopen the socket (INV-APP-020: one start per stream instance).
  const latest = useRef({ client, cursor, context });
  useEffect(() => {
    latest.current = { client, cursor, context };
  }, [client, cursor, context]);

  useEffect(() => {
    let state: EventState = initialEventState(syncEventVersion, latest.current.cursor.read());
    // configure() only freezes version/topics (INV-APP-021); nothing connects
    // until start() below.
    const configured = stream.configure({ syncEventVersion, topics: ['*'] });
    const unsubscribe = stream.onFrame((frame) => {
      const current = latest.current;
      const reduction = reduceEventFrame(state, frame, current.context);
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
