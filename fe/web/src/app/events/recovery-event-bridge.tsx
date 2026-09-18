import { useEffect } from 'react';
import type { QueryClient } from '@tanstack/react-query';
import type { RecoverySession, RecoveryVersion } from '../../systems/recovery/session.ts';
import { useState } from '../../ui/state/public.ts';
import type { EventComposition } from '../composition.ts';
import { EventBridge } from './event-bridge.tsx';

/** One mounted recovery epoch owns one immutable stream/driver. EventBridge
 * remains the sole start/stop owner; React retires it before mounting a successor. */
export function RecoveryEventBridge({ createEvents, recovery, client, version }: Readonly<{
  createEvents: () => EventComposition; recovery: RecoverySession;
  client: QueryClient; version: RecoveryVersion;
}>) {
  const [events] = useState(createEvents);
  useEffect(() => events.stream.onConnectionState(state => recovery.events(state)), [events, recovery]);
  return <EventBridge client={client} stream={events.stream} cursor={events.store}
    syncEventVersion={version.syncEventVersion} dbInstanceId={version.dbInstanceId} />;
}
