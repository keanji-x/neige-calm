// TanStack Query hooks — the single seam between the kernel HTTP API and
// the React tree. Replaces the hand-rolled `useKernel` monolith.
//
// Why hooks (and not a hand-rolled store):
//   - Per-query loading/error state is automatic. No more "the page is
//     stuck on a global spinner because one of N cove fetches is slow".
//   - Request deduplication. Two components asking for `['wave', id]` at
//     the same time share one fetch.
//   - Cache invalidation is declarative. WS events become
//     `queryClient.invalidateQueries({queryKey:[...]})` calls in one
//     place (see `app/eventBridge.tsx`) — Query handles the rest.
//
// Query keys are arrays — pick one shape and never deviate:
//   ['coves']                     — list of all coves
//   ['waves', coveId]             — list of waves in a cove
//   ['wave', waveId]              — full wave detail (cards + overlays)
//
// All queries call the existing `api/calm.ts` functions as their queryFn;
// no fetch logic lives here. Mutations call the same client and follow
// "mutate + invalidate" — optimistic updates can be layered on per-call
// later without changing the public hook surface.

import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from '@tanstack/react-query';
import * as api from './calm';
import type {
  CardPatchBody,
  CovePatchBody,
  KernelCard,
  KernelCove,
  KernelWave,
  KernelWaveDetail,
  NewCardBody,
  NewCoveBody,
  NewWaveBody,
  WavePatchBody,
} from './wire';

// ---------------- Query key factory ----------------
//
// One place to construct keys so the invalidation bridge can't typo a
// key shape relative to the query call site. Importable by eventBridge.

export const queryKeys = {
  coves: () => ['coves'] as const,
  wavesInCove: (coveId: string) => ['waves', coveId] as const,
  waveDetail: (waveId: string) => ['wave', waveId] as const,
};

// ---------------- Query option factories ----------------
//
// Pure `{ queryKey, queryFn }` shapes that both hooks and TanStack Router
// loaders can consume. Loaders call `queryClient.ensureQueryData(opts)`
// before the route component mounts; the component then uses the matching
// `useQuery` hook below which reads the already-cached data instantly,
// eliminating the per-route spinner flash.

export const covesQueryOptions = () => ({
  queryKey: queryKeys.coves(),
  queryFn: () => api.listCoves(),
});

export const wavesByCoveQueryOptions = (coveId: string) => ({
  queryKey: queryKeys.wavesInCove(coveId),
  queryFn: () => api.wavesInCove(coveId),
});

export const waveDetailQueryOptions = (waveId: string) => ({
  queryKey: queryKeys.waveDetail(waveId),
  queryFn: () => api.getWaveDetail(waveId),
});

// ---------------- Queries ----------------

/** All coves. Used by Sidebar, Today calendar, and Cove routing. */
export function useCovesQuery(opts?: Partial<UseQueryOptions<KernelCove[], Error>>) {
  return useQuery<KernelCove[], Error>({
    ...covesQueryOptions(),
    ...opts,
  });
}

/** Waves inside a given cove. Empty `coveId` keeps the query disabled. */
export function useWavesByCoveQuery(
  coveId: string | undefined | null,
  opts?: Partial<UseQueryOptions<KernelWave[], Error>>,
) {
  return useQuery<KernelWave[], Error>({
    ...wavesByCoveQueryOptions(coveId ?? ''),
    enabled: !!coveId,
    ...opts,
  });
}

/** Wave detail (cards + overlays). Disabled when `waveId` falsy. */
export function useWaveDetailQuery(
  waveId: string | undefined | null,
  opts?: Partial<UseQueryOptions<KernelWaveDetail, Error>>,
) {
  return useQuery<KernelWaveDetail, Error>({
    ...waveDetailQueryOptions(waveId ?? ''),
    enabled: !!waveId,
    ...opts,
  });
}

// ---------------- Mutations ----------------
//
// All mutations follow the same shape: call the api client, invalidate the
// affected keys on success, and let WS events handle anything else. WS
// events from the kernel will also invalidate the same keys, but we still
// trigger invalidation client-side because (a) the WS round-trip is async,
// and (b) we want the UI to settle even if the event bus is briefly down.
//
// Optimistic updates are layered on the obvious low-risk wins (title /
// color renames, drag-reorder `sort` patches). Pattern:
//
//   onMutate   → cancelQueries, snapshot cache, write optimistic value,
//                return { previous } so onError can restore.
//   onError    → setQueryData back to the snapshot if we took one.
//   onSettled  → invalidate (runs after both success and error so the
//                rollback path also resyncs from server truth).
//
// Creates and deletes intentionally stay non-optimistic: they hinge on
// server-assigned ids and cascading invalidations, where rollback is much
// more error-prone than the snappiness payoff.

export function useCreateCoveMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCove, Error, NewCoveBody>({
    mutationFn: (body) => api.createCove(body),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
    },
  });
}

/**
 * Update a cove. Optimistic for `name` and `color` patches (the common
 * rename / palette-swap path). If the patch carries any other field
 * (currently only `sort`), we fall through to the plain invalidate-on-
 * settle path — reorder rollback for coves is rare and would require
 * snapshotting + replaying the full list re-sort.
 */
export function useUpdateCoveMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: CovePatchBody };
  type Ctx = { previous: KernelCove[] | null };
  return useMutation<KernelCove, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateCove(id, body),
    onMutate: async ({ id, body }) => {
      const isOptimisticField =
        body.name !== undefined || body.color !== undefined;
      if (!isOptimisticField) return { previous: null };

      const key = queryKeys.coves();
      await qc.cancelQueries({ queryKey: key });
      const previous = qc.getQueryData<KernelCove[]>(key) ?? null;
      if (previous) {
        const now = Date.now();
        qc.setQueryData<KernelCove[]>(
          key,
          previous.map((c) =>
            c.id === id
              ? {
                  ...c,
                  ...(body.name != null ? { name: body.name } : {}),
                  ...(body.color != null ? { color: body.color } : {}),
                  updated_at: now,
                }
              : c,
          ),
        );
      }
      return { previous };
    },
    onError: (_err, _vars, context) => {
      if (context?.previous) {
        qc.setQueryData(queryKeys.coves(), context.previous);
      }
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
    },
  });
}

export function useDeleteCoveMutation() {
  const qc = useQueryClient();
  return useMutation<void, Error, string>({
    mutationFn: (id) => api.deleteCove(id),
    onSuccess: (_v, id) => {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
      // Drop the dead cove's wave list from cache.
      qc.removeQueries({ queryKey: queryKeys.wavesInCove(id) });
    },
  });
}

export function useCreateWaveMutation() {
  const qc = useQueryClient();
  return useMutation<KernelWave, Error, NewWaveBody>({
    mutationFn: (body) => api.createWave(body),
    onSuccess: (wave) => {
      void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
    },
  });
}

/**
 * Update a wave. Optimistic for `title` (rename) and `sort` (drag-reorder
 * within the cove's wave list). Other patch fields (e.g. `archived_at`)
 * stay non-optimistic — archive flips trigger cascading UI moves that are
 * cleaner to drive from the server-confirmed state.
 *
 * Two caches can hold a copy of the wave: the list `['waves', cove_id]`
 * and the detail `['wave', id]`. We update whichever ones are populated,
 * and snapshot both so onError can restore them.
 */
export function useUpdateWaveMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: WavePatchBody };
  type Ctx = {
    previousList: { key: ReturnType<typeof queryKeys.wavesInCove>; value: KernelWave[] } | null;
    previousDetail: KernelWaveDetail | null;
    detailKey: ReturnType<typeof queryKeys.waveDetail>;
  };
  return useMutation<KernelWave, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateWave(id, body),
    onMutate: async ({ id, body }) => {
      const detailKey = queryKeys.waveDetail(id);
      const empty: Ctx = { previousList: null, previousDetail: null, detailKey };
      const isOptimisticField =
        body.title !== undefined || body.sort !== undefined;
      if (!isOptimisticField) return empty;

      // Locate the wave's cove via cached detail first, then fall back to
      // scanning cached wave lists. If neither cache is warm there's
      // nothing to optimistically mutate; we still let the request run.
      const cachedDetail = qc.getQueryData<KernelWaveDetail>(detailKey);
      let listKey: ReturnType<typeof queryKeys.wavesInCove> | null = null;
      if (cachedDetail) {
        listKey = queryKeys.wavesInCove(cachedDetail.wave.cove_id);
      } else {
        const all = qc.getQueriesData<KernelWave[]>({ queryKey: ['waves'] });
        for (const [k, v] of all) {
          if (v && v.some((w) => w.id === id)) {
            listKey = k as ReturnType<typeof queryKeys.wavesInCove>;
            break;
          }
        }
      }

      await qc.cancelQueries({ queryKey: detailKey });
      if (listKey) await qc.cancelQueries({ queryKey: listKey });

      const now = Date.now();
      const applyPatch = (w: KernelWave): KernelWave => ({
        ...w,
        ...(body.title != null ? { title: body.title } : {}),
        ...(body.sort != null ? { sort: body.sort } : {}),
        updated_at: now,
      });

      const ctx: Ctx = { ...empty };

      if (listKey) {
        const previousList = qc.getQueryData<KernelWave[]>(listKey);
        if (previousList) {
          ctx.previousList = { key: listKey, value: previousList };
          qc.setQueryData<KernelWave[]>(
            listKey,
            previousList.map((w) => (w.id === id ? applyPatch(w) : w)),
          );
        }
      }

      if (cachedDetail) {
        ctx.previousDetail = cachedDetail;
        qc.setQueryData<KernelWaveDetail>(detailKey, {
          ...cachedDetail,
          wave: applyPatch(cachedDetail.wave),
        });
      }

      return ctx;
    },
    onError: (_err, _vars, context) => {
      if (!context) return;
      if (context.previousList) {
        qc.setQueryData(context.previousList.key, context.previousList.value);
      }
      if (context.previousDetail) {
        qc.setQueryData(context.detailKey, context.previousDetail);
      }
    },
    onSettled: (wave, _err, vars, context) => {
      // Prefer the server-confirmed cove_id; fall back to whatever list
      // we touched optimistically. Either way we want the detail key
      // invalidated.
      const coveId = wave?.cove_id ?? context?.previousList?.value[0]?.cove_id;
      if (coveId) {
        void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(coveId) });
      }
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(vars.id) });
    },
  });
}

export function useDeleteWaveMutation() {
  const qc = useQueryClient();
  // We need the cove id to invalidate the cove's wave list, so callers
  // pass `{ id, coveId }` — same shape the WS event would carry.
  return useMutation<void, Error, { id: string; coveId: string }>({
    mutationFn: ({ id }) => api.deleteWave(id),
    onSuccess: (_v, { id, coveId }) => {
      void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(coveId) });
      qc.removeQueries({ queryKey: queryKeys.waveDetail(id) });
    },
  });
}

export function useCreateCardMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCard, Error, { waveId: string; body: NewCardBody }>({
    mutationFn: ({ waveId, body }) => api.createCard(waveId, body),
    onSuccess: (card) => {
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(card.wave_id) });
    },
  });
}

/**
 * Update a card. Optimistic only for `sort` — the drag-reorder case
 * within a wave's card grid. `payload` is intentionally NOT optimistic:
 * its shape is per-card-kind (see `cards/*` adapters) and a mid-edit
 * rollback would smear partial state across the card's bespoke UI.
 *
 * The caller doesn't pass `wave_id` in vars, so we discover it by
 * scanning cached wave details for the card. If we can't find it we
 * still send the mutation; onSettled then has no detail key to
 * invalidate and we rely on the WS `card.updated` fanout (see
 * `eventBridge.tsx`, which itself scans for the owning wave).
 */
export function useUpdateCardMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: CardPatchBody };
  type Ctx = {
    detailKey: ReturnType<typeof queryKeys.waveDetail> | null;
    previousDetail: KernelWaveDetail | null;
  };
  return useMutation<KernelCard, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateCard(id, body),
    onMutate: async ({ id, body }) => {
      const empty: Ctx = { detailKey: null, previousDetail: null };
      // Only `sort` is safe to optimistically mirror.
      if (body.sort === undefined || body.sort === null) return empty;

      const entries = qc.getQueriesData<KernelWaveDetail>({ queryKey: ['wave'] });
      let detailKey: ReturnType<typeof queryKeys.waveDetail> | null = null;
      let previousDetail: KernelWaveDetail | null = null;
      for (const [k, v] of entries) {
        if (v && v.cards.some((c) => c.id === id)) {
          detailKey = k as ReturnType<typeof queryKeys.waveDetail>;
          previousDetail = v;
          break;
        }
      }
      if (!detailKey || !previousDetail) return empty;

      await qc.cancelQueries({ queryKey: detailKey });
      const now = Date.now();
      const nextSort = body.sort;
      qc.setQueryData<KernelWaveDetail>(detailKey, {
        ...previousDetail,
        cards: previousDetail.cards.map((c) =>
          c.id === id ? { ...c, sort: nextSort, updated_at: now } : c,
        ),
      });

      return { detailKey, previousDetail };
    },
    onError: (_err, _vars, context) => {
      if (context?.detailKey && context.previousDetail) {
        qc.setQueryData(context.detailKey, context.previousDetail);
      }
    },
    onSettled: (card, _err, _vars, context) => {
      const waveId = card?.wave_id;
      if (waveId) {
        void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
      } else if (context?.detailKey) {
        void qc.invalidateQueries({ queryKey: context.detailKey });
      }
    },
  });
}

export function useDeleteCardMutation() {
  const qc = useQueryClient();
  return useMutation<void, Error, { id: string; waveId: string }>({
    mutationFn: ({ id }) => api.deleteCard(id),
    onSuccess: (_v, { waveId }) => {
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(waveId) });
    },
  });
}
