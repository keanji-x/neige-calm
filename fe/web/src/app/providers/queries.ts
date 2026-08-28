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
  coveFoldersOperation, coveListOperation, createCoveOperation, deleteCoveOperation, sortedCoveFolders,
  sortedCoves, toCove, toCoveFolder, updateCoveOperation, visibleCoves,
  type Cove, type CoveFolder, type CovePatchBody, type NewCoveBody,
} from '../../../../core/domain/cove.ts';
import {
  waveBacklinksOperation, type WaveBacklinks,
} from '../../../../core/domain/report.ts';
import {
  putSettingsOperation, settingsOperation, type SettingsBag, type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import {
  createTerminalCardOperation, createWaveOperation, deleteWaveOperation, overlaysByKindOperation, toWave,
  updateWaveOperation, waveActivityFrom, waveDetailOperation, wavesInCoveOperation,
  type CardWire, type NewTerminalCardBody, type NewWaveBody, type OverlayWire, type Wave,
  type WaveDetailWire, type WavePatchBody,
} from '../../../../core/domain/wave.ts';
import {
  HARNESS_ITEMS_PAGE_LIMIT, harnessItemsOperation, interruptSpecOperation, resetSpecOperation, sendSpecInputOperation,
  specRunOperation, coveConversationsOperation, createCoveConversationOperation,
  type Conversation,
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
  unauthorized: UnauthorizedChannel | undefined,
): Promise<T> {
  const result = await performApiRequest(transport, operation, unauthorized);
  if (result.status === 'failed') throw new ApiError(result.error);
  return result.value;
}

// Key shapes match the legacy app so a cache dump reads the same in both.
export const queryKeys = Object.freeze({
  serverVersion: () => ['server-version'] as const,
  coves: () => ['coves'] as const,
  coveFolders: (coveId: string) => ['cove-folders', coveId] as const,
  wavesInCove: (coveId: string) => ['waves', coveId] as const,
  waveDetail: (waveId: string) => ['wave', waveId] as const,
  waveBacklinks: (waveId: string) => ['wave-backlinks', waveId] as const,
  overlaysByKind: (entityKind: 'wave' | 'card') => ['overlays', entityKind] as const,
  settings: () => ['settings'] as const,
  harnessItems: (cardId: string) => ['harness-items', cardId] as const,
  specRun: (cardId: string) => ['spec-run', cardId] as const,
  /* The event bridge can only invalidate the `['cove-conversations']` prefix —
     no event carries a cove id and no cached row can supply one — so this key
     must keep the cove id in second position for that prefix to reach it. */
  coveConversations: (coveId: string) => ['cove-conversations', coveId] as const,
});

export function harnessItemsQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.harnessItems(cardId),
    queryFn: ({ pageParam }: { pageParam: number }) => runOperation(
      transport, harnessItemsOperation(cardId, pageParam, 'desc'), unauthorized,
    ),
    initialPageParam: 0,
    getNextPageParam: (page: HarnessItem[]) =>
      page.length === HARNESS_ITEMS_PAGE_LIMIT ? page[0]?.id : undefined,
  };
}

export function specRunQueryOptions(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.specRun(cardId),
    queryFn: () => runOperation(transport, specRunOperation(cardId), unauthorized),
  };
}

export function useSpecMutations(transport: ApiTransportPort, cardId: string, unauthorized: UnauthorizedChannel) {
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
    send: (text: string) => runOperation(transport, sendSpecInputOperation(cardId, text), unauthorized).then(refreshAfter),
    interrupt: () => runOperation(transport, interruptSpecOperation(cardId), unauthorized).then(refreshAfter),
    reset: () => runOperation(transport, resetSpecOperation(cardId), unauthorized).then(refreshAfter),
  };
}

export function coveConversationsQueryOptions(
  transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.coveConversations(coveId),
    queryFn: (): Promise<Conversation[]> =>
      runOperation(transport, coveConversationsOperation(coveId), unauthorized),
  };
}

export type CoveConversationMutations = Readonly<{
  /**
   * Mint a conversation and deliver its first message.
   *
   * The key is supplied by the caller because it identifies the *draft*, not
   * the attempt: pressing send again after a timeout must reuse it, or the
   * retry mints a second conversation.
   */
  create: (text: string, idempotencyKey: string) => Promise<Conversation>;
  /** Re-read the list and hand back what it now holds. */
  refresh: () => Promise<Conversation[]>;
}>;

export function useCoveConversationMutations(
  transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel,
): CoveConversationMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: ({ text, idempotencyKey }: { text: string; idempotencyKey: string }) =>
      runOperation(transport, createCoveConversationOperation(coveId, text, idempotencyKey), unauthorized),
    onSuccess: (row) => {
      /* Written through as well as invalidated: the drawer switches to this row
         in the same tick, and a list that does not contain it yet would render
         the panel with no active row until the refetch lands. A replayed key
         answers with a row that is already there, so the write is by id. */
      client.setQueryData<Conversation[]>(queryKeys.coveConversations(coveId), (current) => {
        const rows = current ?? [];
        return rows.some((candidate) => candidate.id === row.id)
          ? rows.map((candidate) => candidate.id === row.id ? row : candidate)
          : [...rows, row];
      });
      void client.invalidateQueries({ queryKey: queryKeys.coveConversations(coveId) });
    },
  });
  return {
    create: (text, idempotencyKey) => create.mutateAsync({ text, idempotencyKey }),
    refresh: () => client.fetchQuery({
      ...coveConversationsQueryOptions(transport, coveId, unauthorized),
      staleTime: 0,
    }),
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

export function coveListQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.coves(),
    queryFn: async (): Promise<Cove[]> =>
      sortedCoves(visibleCoves((await runOperation(transport, coveListOperation(), unauthorized)).map(toCove))),
  };
}

/**
 * The folders a cove has already claimed.
 *
 * Its own cache entry rather than a field on the workspace read: only the
 * new-wave dialog needs it, and folding it into `useWorkspace` would put one
 * request per cove behind every route commit for a fact no route renders.
 */
export function coveFoldersQueryOptions(
  transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel,
) {
  return {
    queryKey: queryKeys.coveFolders(coveId),
    queryFn: async (): Promise<CoveFolder[]> =>
      sortedCoveFolders((await runOperation(transport, coveFoldersOperation(coveId), unauthorized)).map(toCoveFolder)),
  };
}

export type CoveFolders = Readonly<{
  folders: readonly CoveFolder[];
  loading: boolean;
  error: Error | null;
}>;

/**
 * INV-NEWWAVE-002 — `loading` is `!isSuccess`, not merely "no data yet". A
 * 5xx/network failure also leaves `data` undefined; treating that as zero
 * folders would submit `attach_folder: true` for a cove that may already own
 * one. Error is unknown, same as pending.
 */
export function useCoveFolders(
  transport: ApiTransportPort, coveId: string | null, unauthorized: UnauthorizedChannel,
): CoveFolders {
  const query = useQuery({
    ...coveFoldersQueryOptions(transport, coveId ?? '', unauthorized),
    enabled: coveId !== null,
  });
  return {
    folders: query.data ?? [],
    loading: coveId !== null && !query.isSuccess,
    error: coveId !== null && query.error instanceof Error ? query.error : null,
  };
}

export function waveOverlaysQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.overlaysByKind('wave'),
    queryFn: (): Promise<OverlayWire[]> => runOperation(transport, overlaysByKindOperation('wave'), unauthorized),
  };
}

export function wavesInCoveQueryOptions(transport: ApiTransportPort, coveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.wavesInCove(coveId),
    queryFn: async (): Promise<Wave[]> =>
      (await runOperation(transport, wavesInCoveOperation(coveId), unauthorized)).map((wire) => toWave(wire)),
  };
}

export function waveDetailQueryOptions(transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.waveDetail(waveId),
    queryFn: (): Promise<WaveDetailWire> => runOperation(transport, waveDetailOperation(waveId), unauthorized),
  };
}

/**
 * Who cites this wave (§8.3).
 *
 * Its own cache entry rather than a field on the detail: backlinks are written
 * by *other* waves, so they go stale on edits this wave never sees, and folding
 * them into the detail would tie the document's freshness to theirs.
 */
export function waveBacklinksQueryOptions(transport: ApiTransportPort, waveId: string, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.waveBacklinks(waveId),
    queryFn: (): Promise<WaveBacklinks> => runOperation(transport, waveBacklinksOperation(waveId), unauthorized),
  };
}

export function settingsQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return {
    queryKey: queryKeys.settings(),
    queryFn: (): Promise<SettingsBag> => runOperation(transport, settingsOperation(), unauthorized),
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
export function useWorkspace(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Workspace {
  const covesQuery = useQuery(coveListQueryOptions(transport, unauthorized));
  const overlaysQuery = useQuery(waveOverlaysQueryOptions(transport, unauthorized));
  const coves = covesQuery.data ?? [];
  const overlays = overlaysQuery.data ?? [];
  const waveQueries = useQueries({
    queries: coves.map((cove) => wavesInCoveQueryOptions(transport, cove.id, unauthorized)),
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
export function prefetchCoveList(client: QueryClient, transport: ApiTransportPort, unauthorized: UnauthorizedChannel): Promise<Cove[]> {
  return client.ensureQueryData(coveListQueryOptions(transport, unauthorized));
}

// ---------- mutations ----------
//
// Every mutation invalidates. A mutation may additionally write its response
// through to the cache first, but only when that response *is* the new cache
// value (an id-keyed row the server just returned) and the very next render
// needs it — see `useCoveConversationMutations`, where the drawer switches to
// the new row in the same tick. The invalidation still follows and reconciles;
// a write-through that guessed, or that stood in for one, would only widen the
// window in which the cache and the server disagree.

export type CoveMutations = Readonly<{
  create: (body: NewCoveBody) => Promise<Cove>;
  rename: (coveId: string, body: CovePatchBody) => Promise<Cove>;
  remove: (coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useCoveMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): CoveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewCoveBody) => runOperation(transport, createCoveOperation(body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const rename = useMutation({
    mutationFn: ({ coveId, body }: { coveId: string; body: CovePatchBody }) =>
      runOperation(transport, updateCoveOperation(coveId, body), unauthorized),
    onSuccess: () => { void client.invalidateQueries({ queryKey: queryKeys.coves() }); },
  });
  const remove = useMutation({
    mutationFn: ({ coveId, signal }: { coveId: string; signal?: AbortSignal }) =>
      runOperation(transport, { ...deleteCoveOperation(coveId), signal }, unauthorized),
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
  createTerminal: (waveId: string, body: NewTerminalCardBody) => Promise<CardWire>;
  remove: (waveId: string, coveId: string, signal?: AbortSignal) => Promise<void>;
}>;

export function useWaveMutations(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): WaveMutations {
  const client = useQueryClient();
  const create = useMutation({
    mutationFn: (body: NewWaveBody) => runOperation(transport, createWaveOperation(body), unauthorized),
    onSuccess: (wave, body) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
      // Disabled while the dialog is closed, so invalidateQueries would leave
      // a successful cache of `[]` for the next open to paint as 0-folder.
      if (body.attach_folder) {
        client.removeQueries({ queryKey: queryKeys.coveFolders(body.cove_id) });
      }
    },
  });
  const patch = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; coveId: string; body: WavePatchBody }) =>
      runOperation(transport, updateWaveOperation(waveId, body), unauthorized),
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
      runOperation(transport, { ...deleteWaveOperation(waveId), signal }, unauthorized),
    onSuccess: (_result, variables) => {
      client.removeQueries({ queryKey: queryKeys.waveDetail(variables.waveId) });
    },
    // Reconcile both list-derived surfaces even if abort raced a committed DELETE.
    onSettled: (_result, _error, variables) => {
      void client.invalidateQueries({ queryKey: queryKeys.wavesInCove(variables.coveId) });
      void client.invalidateQueries({ queryKey: queryKeys.overlaysByKind('wave') });
    },
  });
  const createTerminal = useMutation({
    mutationFn: ({ waveId, body }: { waveId: string; body: NewTerminalCardBody }) =>
      runOperation(transport, createTerminalCardOperation(waveId, body), unauthorized),
    onSuccess: (card) => {
      client.setQueryData(queryKeys.waveDetail(card.wave_id), (previous: WaveDetailWire | undefined) => {
        if (previous === undefined) return previous;
        if (previous.cards.some((existing) => existing.id === card.id)) return previous;
        return { ...previous, cards: [...previous.cards, card] };
      });
      void client.invalidateQueries({ queryKey: queryKeys.waveDetail(card.wave_id) });
    },
  });
  const patchWave = async (waveId: string, coveId: string, body: WavePatchBody) =>
    toWave(await patch.mutateAsync({ waveId, coveId, body }));
  return {
    create: async (body) => toWave(await create.mutateAsync(body)),
    patch: patchWave,
    createTerminal: async (waveId, body) => createTerminal.mutateAsync({ waveId, body }),
    // `pinned_at` is both the flag and the ordering key, so unpinning is a
    // null write rather than a delete of some separate row.
    setPinned: (waveId, coveId, pinned, nowMs) =>
      patchWave(waveId, coveId, { pinned_at: pinned ? nowMs : null }),
    remove: async (waveId, coveId, signal) => { await remove.mutateAsync({ waveId, coveId, signal }); },
  };
}

export function useSettingsMutation(transport: ApiTransportPort, unauthorized: UnauthorizedChannel): (patch: SettingsPatch) => Promise<SettingsBag> {
  const client = useQueryClient();
  const save = useMutation({
    mutationFn: (patch: SettingsPatch) => runOperation(transport, putSettingsOperation(patch), unauthorized),
    // PUT answers with the full bag, so writing it through avoids a refetch
    // that would briefly render the pre-save values.
    onSuccess: (bag) => { client.setQueryData(queryKeys.settings(), bag); },
  });
  return (patch) => save.mutateAsync(patch);
}
