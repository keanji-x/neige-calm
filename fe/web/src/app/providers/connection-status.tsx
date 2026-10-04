import { createContext, useContext, type ReactNode } from 'react';
import type { ConnectionState } from '../../systems/events/event-stream.ts';
import { useState } from '../../ui/state/public.ts';

export type ConnectionStatus = Readonly<{ connected: boolean; label: string; detail: string; retry?: () => void }>;
const StatusContext = createContext<ConnectionStatus | null>(null);
const StreamContext = createContext<((state: ConnectionState) => void) | null>(null);
export const ConnectionStatusScope = StatusContext.Provider;
export function useConnectionStatus() { return useContext(StatusContext); }
export function usePublishConnectionState() { return useContext(StreamContext); }

/** The event bridge publishes observations; this provider never starts a socket. */
export function LiveConnectionProvider({ children }: Readonly<{ children: ReactNode }>) {
  const [state, setState] = useState<ConnectionState>('disconnected');
  const status = { connected: state === 'connected', label: state === 'connected' ? '已连接' :
    state === 'connecting' ? '正在连接' : '连接已断开', detail: '' };
  return <StreamContext.Provider value={setState}><ConnectionStatusScope value={status}>{children}</ConnectionStatusScope></StreamContext.Provider>;
}
