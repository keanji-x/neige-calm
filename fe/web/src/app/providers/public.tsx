import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';

/**
 * This bundle's view of the negotiated wire contract.
 *
 * Must equal `WEB_COMPAT_VERSION` in `crates/calm-server/src/routes/version.rs`
 * and in `web/src/api/version.ts`. Nothing relates the three at the type level;
 * the `web compat version lockstep gate (#1209 PR-2)` step in
 * `.github/workflows/ci.yml` compares them textually.
 *
 * 16 -> 17: #1209 PR-2 renamed the two template fields of the `POST /api/waves`
 * request body, which `deny_unknown_fields` makes a hard break for older
 * bundles.
 */
export const WEB_COMPAT_VERSION = 17;
export type ServerVersionInfo = Readonly<{ webCompatVersion: number; minWebCompatVersion: number; syncEventVersion: number; dbInstanceId: string }>;
export interface ProviderRuntime {
  fetchVersion(): Promise<ServerVersionInfo>;
  reload(): void;
  deleteDatabase(name: string): void;
  idbDatabaseName: string;
  storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
}

export function retryUnless401(failureCount: number, error: unknown): boolean {
  if (typeof error === 'object' && error !== null && 'failure' in error) {
    const failure = error.failure;
    if (typeof failure === 'object' && failure !== null && 'kind' in failure && failure.kind === 'unauthorized') return false;
  }
  return failureCount < 1;
}

export function AppProviders({ children, runtime, renderEventBridge, cursorStore, client }: {
  children: ReactNode; runtime: ProviderRuntime; renderEventBridge?: (server: ServerVersionInfo) => ReactNode;
  cursorStore: Pick<SyncCursorPort, 'clear'>; client: QueryClient;
}) {
  return <QueryClientProvider client={client}><ThemeProvider storage={runtime.storage}>
    <ServerCompatGate client={client} runtime={runtime} renderEventBridge={renderEventBridge} cursorStore={cursorStore}>{children}</ServerCompatGate>
  </ThemeProvider></QueryClientProvider>;
}

function safeRead(runtime: ProviderRuntime, key: string): string | null { try { return runtime.storage.getItem(key); } catch { return null; } }
function safeWrite(runtime: ProviderRuntime, key: string, value: string): void { try { runtime.storage.setItem(key, value); } catch { /* no-op */ } }
function safeDelete(runtime: ProviderRuntime): void { try { runtime.deleteDatabase(runtime.idbDatabaseName); } catch { /* no-op */ } }

export function ServerCompatGate({ children, runtime, client, renderEventBridge, cursorStore }: {
  children: ReactNode; runtime: ProviderRuntime; client: QueryClient;
  renderEventBridge?: (server: ServerVersionInfo) => ReactNode; cursorStore: Pick<SyncCursorPort, 'clear'>;
}) {
  const [busted, setBusted] = useState(false);
  const [previousInstanceId] = useState(() => safeRead(runtime, DB_INSTANCE_ID_KEY));
  const query = useQuery({ queryKey: ['server-version'], queryFn: () => runtime.fetchVersion(), staleTime: 0, gcTime: 0, retry: retryUnless401, refetchInterval: false }, client);

  useEffect(() => {
    const id = query.data?.dbInstanceId;
    if (!id) return;
    const previous = previousInstanceId;
    if (previous && previous !== id) {
      client.clear(); cursorStore.clear(); safeDelete(runtime);
      safeWrite(runtime, DB_INSTANCE_ID_KEY, id); setBusted(true); runtime.reload(); return;
    }
    if (!previous) safeWrite(runtime, DB_INSTANCE_ID_KEY, id);
  }, [client, cursorStore, previousInstanceId, query.data?.dbInstanceId, runtime]);

  const id = query.data?.dbInstanceId;
  const verdict = id === undefined ? 'pending'
    : previousInstanceId !== null && previousInstanceId !== id ? 'switched' : 'same';
  if (busted) return null;
  if (query.data && query.data.minWebCompatVersion > WEB_COMPAT_VERSION) return <RefreshRequiredOverlay server={query.data} reload={() => runtime.reload()} />;
  return <>{verdict === 'same' && renderEventBridge?.(query.data!)}{children}</>;
}

export function RefreshRequiredOverlay({ server, reload }: { server: ServerVersionInfo; reload: () => void }) {
  return <Dialog open title="Please refresh" onClose={reload} hideTitleRow>
    <section aria-label="Please refresh">
      <h1>Please refresh</h1><p>A new server requires compat v{server.minWebCompatVersion}; this browser provides compat v{WEB_COMPAT_VERSION}.</p>
      <button type="button" onClick={reload}>Refresh now</button>
    </section>
  </Dialog>;
}
