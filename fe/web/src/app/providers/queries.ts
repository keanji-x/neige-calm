// Query wiring shared by app/router and app/shell.
//
// It lives under app/providers rather than under either consumer because the
// router renders the shell and the shell needs the same cove/wave reads: a
// queries module owned by either side would close a cycle that the
// `no-circular` dependency-cruiser rule rejects.

import { useQueries, useQuery, type QueryClient } from '@tanstack/react-query';
import { z } from 'zod';

import { performApiRequest } from '../../../../core/api/client.ts';
import type { ApiFailure, ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import { coveListOperation, sortedCoves, toCove, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import { toWave, wavesInCoveOperation, type Wave } from '../../../../core/domain/wave.ts';
import type { ServerVersionInfo } from './public.tsx';

export class ApiError extends Error {
  readonly failure: ApiFailure;

  constructor(failure: ApiFailure) {
    super(failure.message);
    this.name = 'ApiError';
    this.failure = failure;
  }
}

/** TanStack Query wants a rejected promise; core reports failures as data. */
export async function runOperation<T>(
  transport: ApiTransportPort,
  operation: ApiOperation<T>,
): Promise<T> {
  const result = await performApiRequest(transport, operation);
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

export const queryKeys = Object.freeze({
  serverVersion: () => ['server-version'] as const,
  coves: () => ['coves'] as const,
  wavesInCove: (coveId: string) => ['waves', coveId] as const,
});

const serverVersionSchema = z.object({
  webCompatVersion: z.number(),
  minWebCompatVersion: z.number(),
  syncEventVersion: z.number(),
  dbInstanceId: z.string(),
});

export function serverVersionOperation(): ApiOperation<ServerVersionInfo> {
  return { method: 'GET', path: '/api/version', responseSchema: serverVersionSchema };
}

export function coveListQueryOptions(transport: ApiTransportPort) {
  return {
    queryKey: queryKeys.coves(),
    queryFn: async (): Promise<Cove[]> =>
      sortedCoves(visibleCoves((await runOperation(transport, coveListOperation())).map(toCove))),
  };
}

export function wavesInCoveQueryOptions(transport: ApiTransportPort, coveId: string) {
  return {
    queryKey: queryKeys.wavesInCove(coveId),
    queryFn: async (): Promise<Wave[]> =>
      (await runOperation(transport, wavesInCoveOperation(coveId))).map(toWave),
  };
}

export type Workspace = Readonly<{
  coves: Cove[];
  wavesByCove: ReadonlyMap<string, Wave[]>;
  waves: Wave[];
  covesLoading: boolean;
}>;

/**
 * INV-APP-084 — the cove → waves fan-out is a page-level `useQueries`, never a
 * route loader await. One slow cove must not block the calendar; each cove's
 * list also stays its own cache entry, so a wave moving between coves
 * invalidates two lists instead of the whole workspace.
 */
export function useWorkspace(transport: ApiTransportPort): Workspace {
  const covesQuery = useQuery(coveListQueryOptions(transport));
  const coves = covesQuery.data ?? [];
  const waveQueries = useQueries({
    queries: coves.map((cove) => wavesInCoveQueryOptions(transport, cove.id)),
  });
  const wavesByCove = new Map<string, Wave[]>();
  const waves: Wave[] = [];
  for (const [index, cove] of coves.entries()) {
    const rows = waveQueries[index]?.data ?? [];
    wavesByCove.set(cove.id, rows);
    waves.push(...rows);
  }
  return { coves, wavesByCove, waves, covesLoading: covesQuery.isLoading };
}

/** Route loaders prime only this one list; see INV-APP-084 above. */
export function prefetchCoveList(client: QueryClient, transport: ApiTransportPort): Promise<Cove[]> {
  return client.ensureQueryData(coveListQueryOptions(transport));
}
