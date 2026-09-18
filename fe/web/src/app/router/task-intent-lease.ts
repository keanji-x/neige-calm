import type { QueryClient, QueryKey } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import { admitTransport } from '../providers/recovery-mutation.ts';

/** One explicit submission owns one existing cache entry. Response adoption
 * requires its original admission; releasing local busy state only requires
 * that exact entry and value still belong to it. Neither can recreate logout
 * data or touch a replacement intent, even when the same key is reused. */
export function beginTaskIntent<T>(client: QueryClient, key: QueryKey, transport: ApiTransportPort, previous: T, pending: T) {
  const query = client.getQueryCache().find({ queryKey: key, exact: true });
  if (query === undefined || query.state.data !== previous) return null;
  const admitted = admitTransport(transport);
  const checkpoint = admitted.recovery?.checkpoint();
  let owned = client.setQueryData<T>(key, pending);
  const owns = () => client.getQueryCache().find({ queryKey: key, exact: true }) === query && query.state.data === owned;
  const current = () => {
    if (!owns()) return false;
    try { checkpoint?.(); return true; } catch { return false; }
  };
  const replace = (next: T) => { owned = client.setQueryData<T>(key, next); return true; };
  return {
    transport: admitted, current,
    commit: (next: T) => current() && replace(next),
    release: (next: T) => owns() && replace(next),
  };
}
