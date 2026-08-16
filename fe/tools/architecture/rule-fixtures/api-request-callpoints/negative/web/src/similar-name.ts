export function similarName(performApiRequester: (...args: unknown[]) => unknown): unknown {
  return performApiRequester('/api/session', {}, {});
}
