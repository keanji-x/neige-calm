import type { ApiFailure, ApiOperation, ApiResult, ApiTransportPort } from './types.js';
import type { UnauthorizedChannel } from './unauthorized.js';

function errorText(body: unknown, key: 'code' | 'error', fallback: string): string {
  if (typeof body !== 'object' || body === null) return fallback;
  const value: unknown = (body as Record<string, unknown>)[key];
  return typeof value === 'string' ? value : fallback;
}

function normalizeHttpFailure(status: number, statusText: string, body: unknown): ApiFailure {
  const code = errorText(body, 'code', 'http_error');
  const message = errorText(body, 'error', statusText);
  if (status === 401) return { kind: 'unauthorized', status: 401, code, message, body };
  return { kind: 'http', status, code, message, body };
}

function isTimeoutError(cause: unknown): boolean {
  return typeof cause === 'object' && cause !== null && 'name' in cause && cause.name === 'TimeoutError';
}

/** Executes exactly one transport attempt; retry policy belongs to end-side assembly. */
export async function performApiRequest<T>(
  transport: ApiTransportPort,
  operation: ApiOperation<T>,
  unauthorized?: UnauthorizedChannel,
): Promise<ApiResult<T>> {
  let response;
  try {
    response = await transport.send({
      method: operation.method,
      path: operation.path,
      credentials: 'include',
      ...(operation.signal === undefined ? {} : { signal: operation.signal }),
      ...(operation.body === undefined ? {} : {
        headers: { 'content-type': 'application/json' },
        body: operation.body,
      }),
    });
  } catch (cause) {
    const message = isTimeoutError(cause)
      ? 'Request timed out.'
      : 'Transport request failed';
    return { status: 'failed', error: { kind: 'transport', message, cause } };
  }

  if (response.status < 200 || response.status >= 300) {
    const error = normalizeHttpFailure(response.status, response.statusText, response.body);
    if (error.kind === 'unauthorized') unauthorized?.notify();
    return {
      status: 'failed',
      error,
    };
  }

  const parsed = operation.responseSchema.safeParse(response.status === 204 ? undefined : response.body);
  if (!parsed.success) {
    return {
      status: 'failed',
      error: { kind: 'decode', message: 'API response did not match its schema', cause: parsed.error },
    };
  }
  return { status: 'ready', value: parsed.data };
}
