// The browser half of core/api's injected transport port. core never touches
// `fetch`; this is the only place the web end hands it one.

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';

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

export function createFetchTransport(): ApiTransportPort {
  return {
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
          /*
           * #1505 S6 — a `Uint8Array` body is the bytes themselves.
           *
           * `POST /planner/attachments` takes a raw image, and
           * `JSON.stringify` of a typed array is `{"0":137,"1":80,...}` —
           * a body that is neither the file nor an error, which the server
           * would reject as not-an-image and no type would have caught. The
           * `content-type` still comes from the operation's own headers, which
           * `core/api/client` merges over the `application/json` it adds for
           * any body.
           */
          ...(request.body === undefined ? {} : {
            body: request.body instanceof Uint8Array
              ? (request.body as unknown as BodyInit)
              : JSON.stringify(request.body),
          }),
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
