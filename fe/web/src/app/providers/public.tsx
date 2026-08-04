import { QueryClient, QueryClientProvider, useIsRestoring, useQuery } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { DB_INSTANCE_ID_KEY, SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';

export const WEB_COMPAT_VERSION = 16;
export const PERSIST_OPTIONS = Object.freeze({
  idbDatabaseName: 'neige-calm',
  dbInstanceKey: DB_INSTANCE_ID_KEY,
  syncCursorKey: SYNC_CURSOR_KEY,
  maxAgeMs: 24 * 60 * 60 * 1000,
});

export type ServerVersionInfo = Readonly<{ webCompatVersion: number; minWebCompatVersion: number; syncEventVersion: number; dbInstanceId: string }>;
export interface ProviderRuntime {
  fetchVersion(): Promise<ServerVersionInfo>;
  reload(): void;
  deleteDatabase(name: string): void;
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
}

export function retryUnless401(failureCount: number, error: unknown): boolean {
  if (typeof error === 'object' && error !== null && 'status' in error && error.status === 401) return false;
  return failureCount < 1;
}

export const queryClient = new QueryClient({ defaultOptions: { queries: {
  staleTime: 30_000, gcTime: PERSIST_OPTIONS.maxAgeMs, retry: retryUnless401, refetchOnWindowFocus: false,
} } });

export function QueryRestoreGate({ children, restoring }: { children: ReactNode; restoring?: boolean }) {
  const contextRestoring = useIsRestoring();
  return (restoring ?? contextRestoring) ? null : children;
}

export function AppProviders({ children, runtime, renderEventBridge, client = queryClient, restoring }: {
  children: ReactNode; runtime: ProviderRuntime; renderEventBridge?: (syncEventVersion: number) => ReactNode; client?: QueryClient; restoring?: boolean;
}) {
  return <QueryClientProvider client={client}><QueryRestoreGate restoring={restoring}><ThemeProvider storage={runtime.storage}>
    <ServerCompatGate client={client} runtime={runtime} renderEventBridge={renderEventBridge}>{children}</ServerCompatGate>
  </ThemeProvider></QueryRestoreGate></QueryClientProvider>;
}

function safeRead(runtime: ProviderRuntime, key: string): string | null { try { return runtime.storage.getItem(key); } catch { return null; } }
function safeWrite(runtime: ProviderRuntime, key: string, value: string): void { try { runtime.storage.setItem(key, value); } catch { /* no-op */ } }
function safeRemove(runtime: ProviderRuntime, key: string): void { try { runtime.storage.removeItem(key); } catch { /* no-op */ } }
function safeDelete(runtime: ProviderRuntime): void { try { runtime.deleteDatabase(PERSIST_OPTIONS.idbDatabaseName); } catch { /* no-op */ } }

export function ServerCompatGate({ children, runtime, client, renderEventBridge, restoring = false }: {
  children: ReactNode; runtime: ProviderRuntime; client: QueryClient; renderEventBridge?: (syncEventVersion: number) => ReactNode; restoring?: boolean;
}) {
  const [busted, setBusted] = useState(false);
  const query = useQuery({ queryKey: ['server-version'], queryFn: () => runtime.fetchVersion(), staleTime: 0, gcTime: 0, retry: retryUnless401, refetchInterval: false }, client);

  useEffect(() => {
    const id = query.data?.dbInstanceId;
    if (!id) return;
    const previous = safeRead(runtime, PERSIST_OPTIONS.dbInstanceKey);
    if (previous && previous !== id) {
      client.clear(); safeRemove(runtime, PERSIST_OPTIONS.syncCursorKey); safeDelete(runtime);
      safeWrite(runtime, PERSIST_OPTIONS.dbInstanceKey, id); setBusted(true); runtime.reload(); return;
    }
    if (!previous) safeWrite(runtime, PERSIST_OPTIONS.dbInstanceKey, id);
  }, [client, query.data?.dbInstanceId, runtime]);

  if (restoring || busted) return null;
  if (query.data && query.data.minWebCompatVersion > WEB_COMPAT_VERSION) return <RefreshRequiredOverlay server={query.data} reload={() => runtime.reload()} />;
  return <>{query.data && renderEventBridge?.(query.data.syncEventVersion)}{children}</>;
}

export function RefreshRequiredOverlay({ server, reload }: { server: ServerVersionInfo; reload: () => void }) {
  return <Dialog open title="Please refresh" onClose={reload} hideTitleRow>
    <button type="button" aria-label="Refresh backdrop" data-testid="refresh-overlay" data-nc-refresh-overlay onClick={reload}
      style={{ position: 'fixed', inset: 0, zIndex: 1000, border: 0, background: 'var(--overlay-scrim, rgb(0 0 0 / 55%))' }} />
    <section aria-label="Please refresh" style={{ position: 'fixed', zIndex: 1001, top: '50%', left: '50%', transform: 'translate(-50%, -50%)', width: 'min(36rem, calc(100vw - 48px))', padding: 24, borderRadius: 8, background: 'var(--paper, Canvas)', color: 'var(--text, CanvasText)' }}>
      <h1>Please refresh</h1><p>A new server requires compat v{server.minWebCompatVersion}; this browser provides compat v{WEB_COMPAT_VERSION}.</p>
      <button type="button" onClick={reload}>Refresh now</button>
    </section>
  </Dialog>;
}
