// Query and mutation wiring shared by app/router, app/shell and every feature
// slice.
//
// It lives under app/providers rather than under any one consumer because the
// router renders the shell and both need the same cove/wave reads: a queries
// module owned by either side would close a cycle that the `no-circular`
// dependency-cruiser rule rejects. Feature slices receive the resulting data
// and callbacks as props — `features/**` must not import `app/**`.

import {
  useMutation, useQueries, useQuery, useQueryClient, type QueryClient,
} from '@tanstack/react-query';
import { z } from 'zod';

import { performApiRequest } from '../../../../core/api/client.ts';
import type { ApiFailure, ApiOperation, ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import {
  coveListOperation, createCoveOperation, deleteCoveOperation, sortedCoves, toCove,
  updateCoveOperation, visibleCoves,
  type Cove, type CovePatchBody, type NewCoveBody,
} from '../../../../core/domain/cove.ts';
import {
  waveBacklinksOperation, type WaveBacklinks,
} from '../../../../core/domain/report.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  createWaveOperation, deleteWaveOperation, overlaysByKindOperation, toWave, updateWaveOperation,
  waveActivityFrom, waveDetailOperation, wavesInCoveOperation,
  type NewWaveBody, type OverlayWire, type Wave, type WaveDetailWire, type WavePatchBody,
} from '../../../../core/domain/wave.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptSpecOperation, resetSpecOperation, sendSpecInputOperation,
  specRunOperation,
} from '../../../../core/domain/conversation.ts';
import type { ServerVersionInfo } from './public.tsx';
import type { HarnessItem } from '../../../../core/api/generated/wire.ts';

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
  broadcastUnauthorized = true,
): Promise<T> {
  const unauthorized = broadcastUnauthorized && 'unauthorized' in transport
    ? transport.unauthorized as UnauthorizedChannel : undefined;
  const result = await performApiRequest(transport, operation, unauthorized);
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

// Key shapes match the legacy app so a cache dump reads the same in both.
export const queryKeys = Object.freeze({
  serverVersion: () => ['server-version'] as const,
  coves: () => ['coves'] as const,
  wavesInCove: (coveId: string) => ['waves', coveId] as const,
  waveDetail: (waveId: string) => ['wave', waveId] as const,
  waveBacklinks: (waveId: string) => ['wave-backlinks', waveId] as const,
  overlaysByKind: (entityKind: 'wave' | 'card') => ['overlays', entityKind] as const,
  settings: () => ['settings'] as const,
  harnessItems: (cardId: string) => ['harness-items', cardId] as const,
  specRun: (cardId: string) => ['spec-run', cardId] as const,
});

export function harnessItemsQueryOptions(transport: ApiTransportPort, cardId: string) {
  return {
    queryKey: queryKeys.harnessItems(cardId),
    queryFn: ({ pageParam }: { pageParam: number }) => runOperation(
      transport, harnessItemsOperation(cardId, pageParam, 'desc'),
    ),
    initialPageParam: 0,
    getNextPageParam: (page: HarnessItem[]) =>
      page.length === HARNESS_ITEMS_PAGE_LIMIT ? page[0]?.id : undefined,
  };
}

export function specRunQueryOptions(transport: ApiTransportPort, cardId: string) {
  return {
    queryKey: queryKeys.specRun(cardId),
    queryFn: () => runOperation(transport, specRunOperation(cardId)),
  };
}

export function useSpecMutations(transport: ApiTransportPort, cardId: string) {
  const client = useQueryClient();
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: queryKeys.harnessItems(cardId) }),
    client.invalidateQueries({ queryKey: queryKeys.specRun(cardId) }),
  ]).then(() => undefined);
  const refreshAfter = async <T,>(result: T): Promise<T> => {
    await refresh();
    return result;
  };
  return {
    send: (text: string) => runOperation(transport, sendSpecInputOperation(cardId, text)).then(refreshAfter),
    interrupt: () => runOperation(transport, interruptSpecOperation(cardId)).then(refreshAfter),
    reset: () => runOperation(transport, resetSpecOperation(cardId)).then(refreshAfter),
  };
}

const serverVersionSchema = z.object({
  webCompatVersion: z.number(),
  minWebCompatVersion: z.number(),
  syncEventVersion: z.number(),
  dbInstanceId: z.string(),
});

export function serverVersionOperation(): ApiOperation<ServerVersionInfo> {
  return { method: 'GET', path: '/api/version', responseSchema: serverVersionSchema };
}

/** Sign-out is a server-side session kill; the caller reloads afterwards so
 *  every persisted cache restarts from an unauthenticated probe. */
export function logoutOperation(): ApiOperation<undefined> {
  return { method: 'POST', path: '/api/auth/logout', responseSchema: z.undefined() };
}

// ---------- reads ----------

export function coveListQueryOptions(transport: ApiTransportPort) {
  return {
    queryKey: queryKeys.coves(),
    queryFn: async (): Promise<Cove[]> =>
      sortedCoves(visibleCoves((await runOperation(transport, coveListOperation())).map(toCove))),
  };
}

export function waveOverlaysQueryOptions(transport: ApiTransportPort) {
  return {
    queryKey: queryKeys.overlaysByKind('wave'),
    queryFn: (): Promise<OverlayWire[]> => runOperation(transport, overlaysByKindOperation('wave')),
  };
}

export function wavesInCoveQueryOptions(transport: ApiTransportPort, coveId: string) {
  return {
    queryKey: queryKeys.wavesInCove(coveId),
    queryFn: async (): Promise<Wave[]> =>
      (await runOperation(transport, wavesInCoveOperation(coveId))).map((wire) => toWave(wire)),
  };
}

export function waveDetailQueryOptions(transport: ApiTransportPort, waveId: string) {
  return {
    queryKey: queryKeys.waveDetail(waveId),
    queryFn: (): Promise<WaveDetailWire> => runOperation(transport, waveDetailOperation(waveId)),
  };
}

/**
 * Who cites this wave (§8.3).
 *
 * Its own cache entry rather than a field on the detail: backlinks are written
 * by *other* waves, so they go stale on edits this wave never sees, and folding
 * them into the detail would tie the document's freshness to theirs.
 */
export function waveBacklinksQueryOptions(transport: ApiTransportPort, waveId: string) {
  return {
    queryKey: queryKeys.waveBacklinks(waveId),
    queryFn: (): Promise<WaveBacklinks> => runOperation(transport, waveBacklinksOperation(waveId)),
  };
}

export function settingsQueryOptions(transport: ApiTransportPort) {
  return {
    queryKey: queryKeys.settings(),
    queryFn: (): Promise<SettingsBag> => runOperation(transport, settingsOperation()),
  };
}

export type Workspace = Readonly<{
  coves: Cove[];
  wavesByCove: ReadonlyMap<string, Wave[]>;
  waves: Wave[];
  covesLoading: boolean;
  overlaysLoading: boolean;
  covesError: Error | null;
  overlaysError: Error | null;
  waveErrorsByCove: ReadonlyMap<string, Error>;
  wavesLoadingByCove: ReadonlyMap<string, boolean>;
  retryCoves: () => void;
  retryOverlays: () => void;
  retryWaves: (coveId: string) => void;
}>;

/**
 * INV-APP-084 — the cove → waves fan-out is a page-level `useQueries`, never a
 * route loader await. One slow cove must not block the calendar; each cove's
 * list also stays its own cache entry, so a wave moving between coves
 * invalidates two lists instead of the whole workspace.
 *
 * The workspace-wide wave-overlay read is folded in here so every surface —
 * sidebar buckets, Today's counters, cove lists — sees the same
 * `anyCardNeedsInput` / progress / eta / now, rather than only the wave the
 * user happens to have open.
 */
export function useWorkspace(transport: ApiTransportPort): Workspace {
  const covesQuery = useQuery(coveListQueryOptions(transport));
  const overlaysQuery = useQuery(waveOverlaysQueryOptions(transport));
  const coves = covesQuery.data ?? [];
  const overlays = overlaysQuery.data ?? [];
  const waveQueries = useQueries({
    queries: coves.map((cove) => wavesInCoveQueryOptions(transport, cove.id)),
  });
  const wavesByCove = new Map<string, Wave[]>();
  const waveErrorsByCove = new Map<string, Error>();
  const wavesLoadingByCove = new Map<string, boolean>();
  const waves: Wave[] = [];
  for (const [index, cove] of coves.entries()) {
    const query = waveQueries[index];
    wavesLoadingByCove.set(cove.id, query?.isLoading ?? false);
    if (query?.error instanceof Error) waveErrorsByCove.set(cove.id, query.error);
    if (query?.data !== undefined) {
      const rows = query.data.map((wave) => ({ ...wave, ...waveActivityFrom(wave.id, overlays) }));
      wavesByCove.set(cove.id, rows);
      waves.push(...rows);
    }
  }
  return {
    coves, wavesByCove, waves, covesLoading: covesQuery.isLoading,
    overlaysLoading: overlaysQuery.isLoading,
    covesError: covesQuery.error instanceof Error ? covesQuery.error : null,
    overlaysError: overlaysQuery.error instanceof Error ? overlaysQuery.error : null,
    waveErrorsByCove,
    wavesLoadingByCove,
    retryCoves: () => { void covesQuery.refetch(); },
    retryOverlays: () => { void overlaysQuery.refetch(); },
    retryWaves: (coveId) => {
      const index = coves.findIndex((cove) => cove.id === coveId);
      if (index >= 0) void waveQueries[index]?.refetch();
    },
  };
}

/** Route loaders prime only this one list; see INV-APP-084 above. */
export function prefetchCoveList(client: QueryClient, transport: ApiTransportPort): Promise<Cove[]> {
  return client.ensureQueryData(coveListQueryOptions(transport));
}

// ---------- mutations ----------
//
// Every mutation invalidates rather than hand-patching the cache. The event
// bridge invalidates the same keys when the server pushes, so a hand-patch
// would only widen the window in which the two disagree.

export type CoveMutations = Readonly<{
  create: (body: NewCoveBody) => Promise<Cove>;
  rename: (coveId: string, body: CovePatchBody) => Promise<Cove>;
  remove: (coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useCoveMutations(transport: ApiTransportPort): CoveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewCoveBody) => runOperation(transport, createCoveOperation(body)),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const rename = useMutation({
    mutationFn: ({ coveId, body }: { coveId: string; body: CovePatchBody }) =>
      runOperation(transport, updateCoveOperation(coveId, body)),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const remove = useMutation({
    mutationFn: ({ coveId, signal }: { coveId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteCoveOperation(coveId), signal }),
    onSuccess: (_result, { coveId }) => {
      // The cove is gone; its wave list can never resolve again, so drop it
      // instead of leaving a permanently-stale entry behind.
      client.removeQueries({ queryKey: queryKeys.wavesInCove(coveId) });
    },
    // Abort only ends the client wait: the server may already have committed.
    onSettled: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  return {
    create: async (body) => toCove(await create.mutateAsync(body)),
    rename: async (coveId, body) => toCove(await rename.mutateAsync({ coveId, body })),
    remove: async (coveId, signal) => { await remove.mutateAsync({ coveId, signal }); },
  };
}

export type WaveMutations = Readonly<{
  create: (body: NewWaveBody) => Promise<Wave>;
  patch: (waveId: string, coveId: string, body: WavePatchBody) => Promise<Wave>;
  setPinned: (waveId: string, coveId: string, pinned: boolean, nowMs: number) => Promise<Wave>;
  remove: (waveId: string, coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useWaveMutations(transport: ApiTransportPort): WaveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewWaveBody) => runOperation(transport, createWaveOperation(body)),
    onSuccess: (wave) => { void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) }); },
  });
  const patch = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; coveId: string; body: WavePatchBody }) =>
      runOperation(transport, updateWaveOperation(waveId, body)),
    onSuccess: (wave, variables) => {
      // Prefer the cove the server just reported: a patch can move the wave.
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
      if (wave.cove_id !== variables.coveId) {
        void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(variables.coveId) });
      }
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
  });
  const remove = useMutation({
    mutationFn: ({ waveId, signal }: { waveId: string; coveId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteWaveOperation(waveId), signal }),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(variables.coveId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
    },
  });
  const patchWave = async (waveId: string, coveId: string, body: WavePatchBody) =>
    toWave(await patch.mutateAsync({ waveId, coveId, body }));
  return {
    create: async (body) => toWave(await create.mutateAsync(body)),
    patch: patchWave,
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (waveId, coveId, pinned, nowMs) =>
      patchWave(waveId, coveId, { pinned_at: pinned ? nowMs : null }),
    remove: async (waveId, coveId, signal) => { await remove.mutateAsync({ waveId, coveId, signal }); },
  };
}

export function useSettingsMutation(transport: ApiTransportPort): (patch: SettingsPatch) => Promise<SettingsBag> {
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (patch: SettingsPatch) => runOperation(transport, putSettingsOperation(patch)),
    // PUT answers with the full bag, so writing it through avoids a refetch
    // that would briefly render the pre-save values.
    onSuccess: (bag) => { client.setQueryData(queryKeys.settings(), bag); },
  });
  return (patch) => save.mutateAsync(patch);
}
