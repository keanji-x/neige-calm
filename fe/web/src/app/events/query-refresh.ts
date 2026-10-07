/** Query-client surface needed to replace an in-flight read before invalidating it. */
export interface QueryRefreshPort {
  cancelQueries(filters: { queryKey: readonly unknown[] }): Promise<unknown>;
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
}

/**
 * TanStack may reuse an in-flight initial fetch after invalidation. Abort that request first, then
 * invalidate either the same query or the whole cache so the replacement read owns the final value.
 * A refresh is reconciliation: a read that fails shows on its query, and never escapes as a rejection.
 * Settles once the replacement read has.
 */
export function cancelThenInvalidate(
  client: QueryRefreshPort,
  cancelKey: readonly unknown[],
  invalidateKey: readonly unknown[] | null = cancelKey,
): Promise<void> {
  const invalidate = () => Promise.resolve(invalidateKey === null
    ? client.invalidateQueries()
    : client.invalidateQueries({ queryKey: invalidateKey })).catch(() => undefined);
  return client.cancelQueries({ queryKey: cancelKey }).then(invalidate, invalidate).then(() => undefined);
}
