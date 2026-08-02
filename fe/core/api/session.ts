import type { ApiFailure, ApiResult } from './types.js';

export type SessionProbeState<T> =
  | Readonly<{ status: 'unknown' }>
  | Readonly<{ status: 'authed'; value: T }>
  | Readonly<{ status: 'unauthed' }>
  | Readonly<{ status: 'error'; error: Exclude<ApiFailure, { kind: 'unauthorized' }> }>;

/** Converts an absent/in-flight result and the normalized API channel into session lifecycle data. */
export function resolveSessionProbe<T>(result: ApiResult<T> | undefined): SessionProbeState<T> {
  if (result === undefined) return { status: 'unknown' };
  if (result.status === 'ready') return { status: 'authed', value: result.value };
  if (result.error.kind === 'unauthorized') return { status: 'unauthed' };
  return { status: 'error', error: result.error };
}
