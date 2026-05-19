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

// ---------------- Queries ----------------

/** All coves. Used by Sidebar, Today calendar, and Cove routing. */
export function useCovesQuery(opts?: Partial<UseQueryOptions<KernelCove[], Error>>) {
  return useQuery<KernelCove[], Error>({
    queryKey: queryKeys.coves(),
    queryFn: () => api.listCoves(),
    ...opts,
  });
}

/** Waves inside a given cove. Empty `coveId` keeps the query disabled. */
export function useWavesByCoveQuery(
  coveId: string | undefined | null,
  opts?: Partial<UseQueryOptions<KernelWave[], Error>>,
) {
  return useQuery<KernelWave[], Error>({
    queryKey: queryKeys.wavesInCove(coveId ?? ''),
    queryFn: () => api.wavesInCove(coveId as string),
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
    queryKey: queryKeys.waveDetail(waveId ?? ''),
    queryFn: () => api.getWaveDetail(waveId as string),
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

export function useCreateCoveMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCove, Error, NewCoveBody>({
    mutationFn: (body) => api.createCove(body),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.coves() });
    },
  });
}

export function useUpdateCoveMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCove, Error, { id: string; body: CovePatchBody }>({
    mutationFn: ({ id, body }) => api.updateCove(id, body),
    onSuccess: () => {
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

export function useUpdateWaveMutation() {
  const qc = useQueryClient();
  return useMutation<KernelWave, Error, { id: string; body: WavePatchBody }>({
    mutationFn: ({ id, body }) => api.updateWave(id, body),
    onSuccess: (wave) => {
      void qc.invalidateQueries({ queryKey: queryKeys.wavesInCove(wave.cove_id) });
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(wave.id) });
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

export function useUpdateCardMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCard, Error, { id: string; body: CardPatchBody }>({
    mutationFn: ({ id, body }) => api.updateCard(id, body),
    onSuccess: (card) => {
      void qc.invalidateQueries({ queryKey: queryKeys.waveDetail(card.wave_id) });
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
