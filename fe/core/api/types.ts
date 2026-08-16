import type { z } from 'zod';

/** Platform-neutral subset used to relay request cancellation to an adapter. */
export type ApiAbortSignal = Readonly<{
  aborted: boolean;
  addEventListener(type: 'abort', listener: () => void, options?: { once?: boolean }): void;
  removeEventListener(type: 'abort', listener: () => void): void;
}>;

export type ApiMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

/** Platform adapters must preserve session cookies on every API request. */
export type ApiRequest = Readonly<{
  method: ApiMethod;
  path: string;
  credentials: 'include';
  headers?: Readonly<Record<string, string>>;
  body?: unknown;
  signal?: ApiAbortSignal;
}>;

export type ApiTransportResponse = Readonly<{
  status: number;
  statusText: string;
  body: unknown;
}>;

/** Browser, native, and test ends implement transport; core never calls a platform API. */
export interface ApiTransportPort {
  send(request: ApiRequest): Promise<ApiTransportResponse>;
}

export type UnauthorizedFailure = Readonly<{
  kind: 'unauthorized';
  status: 401;
  code: string;
  message: string;
  body?: unknown;
}>;

export type HttpFailure = Readonly<{
  kind: 'http';
  status: number;
  code: string;
  message: string;
  body?: unknown;
}>;

export type TransportFailure = Readonly<{
  kind: 'transport';
  message: string;
  cause?: unknown;
}>;

export type ApiDecodeFailure = Readonly<{
  kind: 'decode';
  message: string;
  cause?: unknown;
}>;

export type ApiFailure = UnauthorizedFailure | HttpFailure | TransportFailure | ApiDecodeFailure;

/** Mirrors core/state's data-valued `status: failed` error channel. */
export type ApiResult<T> =
  | Readonly<{ status: 'ready'; value: T }>
  | Readonly<{ status: 'failed'; error: ApiFailure }>;

export type ApiOperation<T> = Readonly<{
  method: ApiMethod;
  path: string;
  responseSchema: z.ZodType<T>;
  body?: unknown;
  /**
   * Request headers the operation itself needs, merged over the `content-type`
   * the client adds for a body.
   *
   * Here rather than folded into `body` because `POST
   * /api/coves/{id}/conversations` takes `Idempotency-Key` as a *header* and
   * rejects the request without it (400) — an operation that cannot express a
   * header cannot call it at all.
   */
  headers?: Readonly<Record<string, string>>;
  signal?: ApiAbortSignal;
}>;
