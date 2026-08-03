import type { z } from 'zod';

export type ApiMethod = 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';

/** Platform adapters must preserve session cookies on every API request. */
export type ApiRequest = Readonly<{
  method: ApiMethod;
  path: string;
  credentials: 'include';
  headers?: Readonly<Record<string, string>>;
  body?: unknown;
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
}>;
