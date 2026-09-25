import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import { DATABASE_ID_KEY, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import type { SyncCursorPort } from '../../systems/events/cursor-port.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { BundledConnectionNotice } from '../auth/bundled-connection.tsx';
import { ReadReceiptScopeProvider } from './ui-preferences.tsx';
import styles from './preflight-status.module.css';

/**
 * This bundle's view of the negotiated wire contract. Must equal `WEB_COMPAT_VERSION` in
 * `crates/calm-server/src/routes/version.rs` and `web/src/api/version.ts`; CI compares them textually.
 */
export const WEB_COMPAT_VERSION = 31;
/** `databaseId` / `nowMs` are optional in the type so an older kernel still parses before the curtain decides; absent means a `null` receipt scope, under which nothing is ever unread. */
export type ServerVersionInfo = Readonly<{ conversationCreateModel?: boolean; webCompatVersion: number; minWebCompatVersion: number; syncEventVersion: number; dbInstanceId: string; databaseId?: string; nowMs?: number }>;
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

export function ServerCompatGate({ children: routeContent, runtime, client, renderEventBridge, cursorStore }: {
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

  /* The database's STABLE id, remembered like the instance id but with none of the cache busting. */
  const databaseId = query.data?.databaseId;
  useEffect(() => {
    if (databaseId === undefined) return;
    if (safeRead(runtime, DATABASE_ID_KEY) !== databaseId) safeWrite(runtime, DATABASE_ID_KEY, databaseId);
  }, [databaseId, runtime]);

  const id = query.data?.dbInstanceId;
  const verdict = id === undefined ? 'pending'
    : previousInstanceId !== null && previousInstanceId !== id ? 'switched' : 'same';
  // The receipt scope is the database identity (never the per-boot instance id) plus the server clock;
  // a kernel that reports neither leaves the scope null, and a null scope is never unread.
  const nowMs = query.data?.nowMs;
  const receiptScope = verdict === 'same' && databaseId !== undefined && nowMs !== undefined
    ? { id: databaseId, nowMs } : { id: null, nowMs: null };
  const children = <ReadReceiptScopeProvider id={receiptScope.id} nowMs={receiptScope.nowMs}>{routeContent}</ReadReceiptScopeProvider>;
  if (busted) return __NC_BUNDLED__ ? <BundledConnectionNotice kind="checking" /> : null;
  if (__NC_BUNDLED__ && query.data === undefined) return <BundledConnectionNotice kind={query.isError || query.fetchStatus === 'paused' ? 'unreachable' : 'checking'}>
    {(query.isError || query.fetchStatus === 'paused') && <button type="button" disabled={query.isFetching} onClick={() => { void query.refetch(); }}>重试连接</button>}
  </BundledConnectionNotice>;
  if (__NC_BUNDLED__ && query.data && query.data.webCompatVersion < WEB_COMPAT_VERSION) return <BundledConnectionNotice kind="server-update" />;
  if (__NC_BUNDLED__ && query.data && query.data.minWebCompatVersion > WEB_COMPAT_VERSION) return <BundledConnectionNotice kind="app-update" />;
  if (query.data && query.data.minWebCompatVersion > WEB_COMPAT_VERSION) return <RefreshRequiredOverlay server={query.data} reload={() => runtime.reload()} />;
  return <>{verdict === 'same' && renderEventBridge?.(query.data!)}{children}
    {query.data === undefined && (query.isError || query.fetchStatus === 'paused') && (
      <div className={styles.status} role="status">
        <span>{query.fetchStatus === 'paused' ? 'Offline · Live updates paused'
          : query.isFetching ? 'Reconnecting live updates…' : 'Live updates unavailable'}</span>
        <button type="button" className={styles.retry} aria-label="Retry live updates"
          title={query.error instanceof Error ? query.error.message : 'Reconnect to restore live updates'}
          disabled={query.isFetching} onClick={() => { void query.refetch(); }}>Retry</button>
      </div>
    )}
  </>;
}

export function RefreshRequiredOverlay({ server, reload }: { server: ServerVersionInfo; reload: () => void }) {
  return <Dialog open title="Please refresh" onClose={reload} hideTitleRow>
    <section aria-label="Please refresh">
      <h1>Please refresh</h1><p>A new server requires compat v{server.minWebCompatVersion}; this browser provides compat v{WEB_COMPAT_VERSION}.</p>
      <button type="button" onClick={reload}>Refresh now</button>
    </section>
  </Dialog>;
}
