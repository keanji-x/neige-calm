// The browser half of core/api's injected transport port. core never touches
// `fetch`; this is the only place the web end hands it one.

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';

const REQUEST_TIMEOUT_MS = 30_000;

async function readBody(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text === '') return undefined;
  try {
    return JSON.parse(text);
  } catch {
    // A non-JSON body is still data: core's failure normalizer reads `code` /
    // `error` off an object and falls back to statusText otherwise.
    return text;
  }
}

export function createFetchTransport(unauthorized?: UnauthorizedChannel): ApiTransportPort {
  return {
    ...(unauthorized === undefined ? {} : { unauthorized }),
    async send(request: ApiRequest): Promise<ApiTransportResponse> {
      const controller = new AbortController();
      const relayAbort = () => controller.abort();
      request.signal?.addEventListener('abort', relayAbort, { once: true });
      if (request.signal?.aborted) relayAbort();
      const timeout = setTimeout(() => controller.abort(new DOMException('Request timed out.', 'TimeoutError')), REQUEST_TIMEOUT_MS);
      try {
        const response = await fetch(request.path, {
          method: request.method,
          credentials: request.credentials,
          ...(request.headers === undefined ? {} : { headers: { ...request.headers } }),
          ...(request.body === undefined ? {} : { body: JSON.stringify(request.body) }),
          signal: controller.signal,
        });
        return { status: response.status, statusText: response.statusText, body: await readBody(response) };
      } finally {
        clearTimeout(timeout);
        request.signal?.removeEventListener('abort', relayAbort);
      }
    },
  };
}
