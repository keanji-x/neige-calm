// The browser half of core/api's injected transport port. core never touches
// `fetch`; this is the only place the web end hands it one.

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';

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
      const response = await fetch(request.path, {
        method: request.method,
        credentials: request.credentials,
        ...(request.headers === undefined ? {} : { headers: { ...request.headers } }),
        ...(request.body === undefined ? {} : { body: JSON.stringify(request.body) }),
      });
      return { status: response.status, statusText: response.statusText, body: await readBody(response) };
    },
  };
}
