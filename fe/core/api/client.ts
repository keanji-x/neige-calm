import type { ApiFailure, ApiOperation, ApiResult, ApiTransportPort } from './types.js';

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

/** Executes exactly one transport attempt; retry policy belongs to end-side assembly. */
export async function performApiRequest<T>(
  transport: ApiTransportPort,
  operation: ApiOperation<T>,
): Promise<ApiResult<T>> {
  let response;
  try {
    response = await transport.send({
      method: operation.method,
      path: operation.path,
      credentials: 'include',
      ...(operation.body === undefined ? {} : {
        headers: { 'content-type': 'application/json' },
        body: operation.body,
      }),
    });
  } catch (cause) {
    return { status: 'failed', error: { kind: 'transport', message: 'Transport request failed', cause } };
  }

  if (response.status < 200 || response.status >= 300) {
    return {
      status: 'failed',
      error: normalizeHttpFailure(response.status, response.statusText, response.body),
    };
  }

  const parsed = operation.responseSchema.safeParse(response.body);
  if (!parsed.success) {
    return {
      status: 'failed',
      error: { kind: 'decode', message: 'API response did not match its schema', cause: parsed.error },
    };
  }
  return { status: 'ready', value: parsed.data };
}
