/** Query-client surface needed to replace an in-flight read before invalidating it. */
export interface QueryRefreshPort {
  cancelQueries(filters: { queryKey: readonly unknown[] }): Promise<unknown>;
  invalidateQueries(filters?: { queryKey?: readonly unknown[] }): unknown;
}

/**
 * TanStack may reuse an in-flight initial fetch after invalidation. Abort that request first, then
 * invalidate either the same query or the whole cache so the replacement read owns the final value.
 */
export function cancelThenInvalidate(
  client: QueryRefreshPort,
  cancelKey: readonly unknown[],
  invalidateKey: readonly unknown[] | null = cancelKey,
): void {
  const invalidate = () => invalidateKey === null
    ? client.invalidateQueries()
    : client.invalidateQueries({ queryKey: invalidateKey });
  void client.cancelQueries({ queryKey: cancelKey }).then(invalidate, invalidate);
}
