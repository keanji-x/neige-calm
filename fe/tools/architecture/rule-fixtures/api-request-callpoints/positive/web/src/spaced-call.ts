export function spacedCall(performApiRequest: (...args: unknown[]) => unknown): unknown {
  return performApiRequest ('/api/session', {}, {});
}
