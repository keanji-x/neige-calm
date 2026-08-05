import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { DB_INSTANCE_ID_KEY, SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';

export const WEB_COMPAT_VERSION = 16;
export type ServerVersionInfo = Readonly<{ webCompatVersion: number; minWebCompatVersion: number; syncEventVersion: number; dbInstanceId: string }>;
export interface ProviderRuntime {
  fetchVersion(): Promise<ServerVersionInfo>;
  reload(): void;
  deleteDatabase(name: string): void;
  idbDatabaseName: string;
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
}

export function retryUnless401(failureCount: number, error: unknown): boolean {
  if (typeof error === 'object' && error !== null && 'status' in error && error.status === 401) return false;
  return failureCount < 1;
}

export function AppProviders({ children, runtime, renderEventBridge, client }: {
  children: ReactNode; runtime: ProviderRuntime; renderEventBridge?: (syncEventVersion: number) => ReactNode; client: QueryClient;
}) {
  return <QueryClientProvider client={client}><ThemeProvider storage={runtime.storage}>
    <ServerCompatGate client={client} runtime={runtime} renderEventBridge={renderEventBridge}>{children}</ServerCompatGate>
  </ThemeProvider></QueryClientProvider>;
}

function safeRead(runtime: ProviderRuntime, key: string): string | null { try { return runtime.storage.getItem(key); } catch { return null; } }
function safeWrite(runtime: ProviderRuntime, key: string, value: string): void { try { runtime.storage.setItem(key, value); } catch { /* no-op */ } }
function safeRemove(runtime: ProviderRuntime, key: string): void { try { runtime.storage.removeItem(key); } catch { /* no-op */ } }
function safeDelete(runtime: ProviderRuntime): void { try { runtime.deleteDatabase(runtime.idbDatabaseName); } catch { /* no-op */ } }

export function ServerCompatGate({ children, runtime, client, renderEventBridge }: {
  children: ReactNode; runtime: ProviderRuntime; client: QueryClient; renderEventBridge?: (syncEventVersion: number) => ReactNode;
}) {
  const [busted, setBusted] = useState(false);
  const query = useQuery({ queryKey: ['server-version'], queryFn: () => runtime.fetchVersion(), staleTime: 0, gcTime: 0, retry: retryUnless401, refetchInterval: false }, client);

  useEffect(() => {
    const id = query.data?.dbInstanceId;
    if (!id) return;
    const previous = safeRead(runtime, DB_INSTANCE_ID_KEY);
    if (previous && previous !== id) {
      client.clear(); safeRemove(runtime, SYNC_CURSOR_KEY); safeDelete(runtime);
      safeWrite(runtime, DB_INSTANCE_ID_KEY, id); setBusted(true); runtime.reload(); return;
    }
    if (!previous) safeWrite(runtime, DB_INSTANCE_ID_KEY, id);
  }, [client, query.data?.dbInstanceId, runtime]);

  if (busted) return null;
  if (query.data && query.data.minWebCompatVersion > WEB_COMPAT_VERSION) return <RefreshRequiredOverlay server={query.data} reload={() => runtime.reload()} />;
  return <>{query.data && renderEventBridge?.(query.data.syncEventVersion)}{children}</>;
}

export function RefreshRequiredOverlay({ server, reload }: { server: ServerVersionInfo; reload: () => void }) {
  return <Dialog open title="Please refresh" onClose={reload} hideTitleRow>
    <section aria-label="Please refresh">
      <h1>Please refresh</h1><p>A new server requires compat v{server.minWebCompatVersion}; this browser provides compat v{WEB_COMPAT_VERSION}.</p>
      <button type="button" onClick={reload}>Refresh now</button>
    </section>
  </Dialog>;
}
