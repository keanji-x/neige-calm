import type { QueryClient } from '@tanstack/react-query';
import type { Area } from '../../../../core/domain/area.ts';
import { queryKeys } from './query-keys.ts';

/** App owns parent/child query assembly; core supplies only a confirmed deletion fact. */
export interface AreaQueryLifecyclePort {
  cancelQueries(filters: { queryKey: readonly unknown[]; exact?: boolean }): Promise<unknown>;
  getQueryData<T>(key: readonly unknown[]): T | undefined;
  setQueryData<T>(key: readonly unknown[], value: T): unknown;
  getQueryCache: QueryClient['getQueryCache'];
}

/**
 * Cancel synchronously before writing: even a transport ignoring abort cannot commit an old parent
 * result. Retire children only after their observers detach from the authoritative parent list.
 * Removing an observed child immediately lets useQueries rebuild it while React still holds the old
 * render. The temporary subscription holds no Area membership and is released on removal/clear.
 */
export function retireAreaQueries(client: AreaQueryLifecyclePort, areaId: string): void {
  void client.cancelQueries({ queryKey: queryKeys.areas() });
  const current = client.getQueryData<readonly Area[]>(queryKeys.areas());
  if (current !== undefined) client.setQueryData(queryKeys.areas(), current.filter(area => area.id !== areaId));
  const cache = client.getQueryCache();
  for (const query of cache.findAll({ queryKey: queryKeys.tracksInArea(areaId) })) {
    // Install before cancellation: its synchronous notifications may detach the last observer.
    const unsubscribe = cache.subscribe(event => {
      if (event.query !== query) return;
      if (event.type === 'removed') unsubscribe();
      else if (event.type === 'observerRemoved' && query.getObserversCount() === 0) {
        unsubscribe();
        cache.remove(query);
      }
    });
    void client.cancelQueries({ queryKey: query.queryKey, exact: true });
    if (query.getObserversCount() === 0) {
      unsubscribe();
      cache.remove(query);
    }
  }
}
