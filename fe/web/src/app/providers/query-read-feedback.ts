import type { UseQueryResult } from '@tanstack/react-query';

/** A first-read retry clears Query's error temporarily; its failure timestamp
 * survives until success. Keep recovery controls mounted through that pending
 * phase without mirroring request state or showing an error on the first load. */
export function hasReadFailure(query: Pick<UseQueryResult, 'isError' | 'isPending' | 'errorUpdatedAt'>): boolean {
  return query.isError || (query.isPending && query.errorUpdatedAt > 0);
}
