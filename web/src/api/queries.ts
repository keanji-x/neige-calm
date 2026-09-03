// TanStack Query hooks — the single seam between the kernel HTTP API and
// the React tree. Replaces the hand-rolled `useKernel` monolith.
//
// Why hooks (and not a hand-rolled store):
//   - Per-query loading/error state is automatic. No more "the page is
//     stuck on a global spinner because one of N area fetches is slow".
//   - Request deduplication. Two components asking for `['track', id]` at
//     the same time share one fetch.
//   - Cache invalidation is declarative. WS events become
//     `queryClient.invalidateQueries({queryKey:[...]})` calls in one
//     place (see `app/eventBridge.tsx`) — Query handles the rest.
//
// Query keys are arrays — pick one shape and never deviate:
//   ['areas']                     — list of all areas
//   ['tracks', areaId]             — list of tracks in an area
//   ['track', trackId]              — full track detail (cards + overlays)
//
// All queries call the existing `api/calm.ts` functions as their queryFn;
// no fetch logic lives here. Mutations call the same client and follow
// "mutate + invalidate" — optimistic updates can be layered on per-call
// later without changing the public hook surface.

import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
  type UseQueryOptions,
} from '@tanstack/react-query';
import * as api from './calm';
import { taskBlockVerdictSchema } from './schemas';
import type {
  CardPatchBody,
  AreaPatchBody,
  KernelCard,
  KernelArea,
  KernelOverlay,
  KernelTrack,
  KernelTrackDetail,
  NewCardBody,
  NewAreaBody,
  NewTrackBody,
  SettingsBag,
  SettingsPutBody,
  TrackPatchBody,
} from './wire';

type TrackFileQueryOpts = {
  enabled?: boolean;
};

// ---------------- Query key factory ----------------
//
// One place to construct keys so the invalidation bridge can't typo a
// key shape relative to the query call site. Importable by eventBridge.

export const queryKeys = {
  areas: () => ['areas'] as const,
  tracksInArea: (areaId: string) => ['tracks', areaId] as const,
  trackDetail: (trackId: string) => ['track', trackId] as const,
  trackReport: (trackId: string) => ['track-report', trackId] as const,
  trackBacklinks: (trackId: string) => ['track-backlinks', trackId] as const,
  trackFiles: (trackId: string) => ['track-files', trackId] as const,
  trackFileList: (trackId: string, path: string | null | undefined) =>
    trackFileListQueryKey(trackId, path),
  trackFileContent: (trackId: string, path: string | null | undefined) =>
    trackFileContentQueryKey(trackId, path),
  /** Global track/card overlay snapshot — populated by the Sidebar so
   *  per-track status indicators stay accurate without detail fetches. */
  overlaysByKind: (entity_kind: 'track' | 'card') =>
    ['overlays', entity_kind] as const,
  /** App-global settings bag (`http_proxy`, `https_proxy`, …). Read by
   *  the Settings page; not invalidated by WS (settings only take effect
   *  on the next codex spawn, so there's no need for live propagation). */
  settings: () => ['settings'] as const,
  /**
   * Issue #250 PR 5 — calendar window query keyed on `[since, until]` in
   * unix ms. Each week (or any user-chosen window) gets its own cache
   * entry; advancing the week re-uses any cached neighbour windows
   * directly. `area_id` is intentionally NOT part of the key because the
   * calendar page never filters by area (issue Non-goals); if we add a
   * filter later it lands here as a third tuple element.
   */
  tracksRange: (since: number, until: number) =>
    ['tracks-range', since, until] as const,
};

export const trackFileListQueryKey = (
  trackId: string,
  path: string | null | undefined,
) => ['track-files', trackId, 'ls', path ?? ''] as const;

export const trackFileContentQueryKey = (
  trackId: string,
  path: string | null | undefined,
) => ['track-files', trackId, 'cat', path ?? ''] as const;

// ---------------- Query option factories ----------------
//
// Pure `{ queryKey, queryFn }` shapes that both hooks and TanStack Router
// loaders can consume. Loaders call `queryClient.ensureQueryData(opts)`
// before the route component mounts; the component then uses the matching
// `useQuery` hook below which reads the already-cached data instantly,
// eliminating the per-route spinner flash.

export const areasQueryOptions = () => ({
  queryKey: queryKeys.areas(),
  queryFn: () => api.listAreas(),
});

export const tracksByAreaQueryOptions = (areaId: string) => ({
  queryKey: queryKeys.tracksInArea(areaId),
  queryFn: () => api.tracksInArea(areaId),
});

export const trackDetailQueryOptions = (trackId: string) => ({
  queryKey: queryKeys.trackDetail(trackId),
  queryFn: () => api.getTrackDetail(trackId),
});

export const trackBacklinksQueryOptions = (trackId: string) => ({
  queryKey: queryKeys.trackBacklinks(trackId),
  queryFn: () => api.getTrackBacklinks(trackId),
});

export const trackReportQueryOptions = (trackId: string) => ({
  queryKey: queryKeys.trackReport(trackId),
  queryFn: async () => {
    const report = await api.getTrackReport(trackId);
    return {
      ...report,
      taskDiagnostics: report.taskDiagnostics.map((verdict) =>
        taskBlockVerdictSchema.parse(verdict)),
    } as api.TrackReportRead;
  },
});

export const overlaysByKindQueryOptions = (entity_kind: 'track' | 'card') => ({
  queryKey: queryKeys.overlaysByKind(entity_kind),
  queryFn: () => api.listAllOverlays(entity_kind),
});

/**
 * Issue #250 PR 5 — calendar window options. The kernel uses inclusive
 * endpoints; the calendar always passes a week-aligned [Mon 00:00, Sun
 * 23:59:59.999] in local time so neighbouring weeks don't share cache
 * entries. `gcTime` caps the per-window cache at 5 minutes so the user
 * paging back and forth through weeks doesn't accrete an unbounded
 * cache (each distinct week is a separate query key); WS events still
 * invalidate the active window in real time.
 */
export const tracksRangeQueryOptions = (since: number, until: number) => ({
  queryKey: queryKeys.tracksRange(since, until),
  queryFn: () => api.tracksRange({ since, until }),
  gcTime: 5 * 60 * 1000,
});

// ---------------- Queries ----------------

/** All areas. Used by Sidebar, Today calendar, and Area routing. */
export function useAreasQuery(opts?: Partial<UseQueryOptions<KernelArea[], Error>>) {
  return useQuery<KernelArea[], Error>({
    ...areasQueryOptions(),
    ...opts,
  });
}

/** Tracks inside a given area. Empty `areaId` keeps the query disabled. */
export function useTracksByAreaQuery(
  areaId: string | undefined | null,
  opts?: Partial<UseQueryOptions<KernelTrack[], Error>>,
) {
  return useQuery<KernelTrack[], Error>({
    ...tracksByAreaQueryOptions(areaId ?? ''),
    enabled: !!areaId,
    ...opts,
  });
}

/** Track detail (cards + overlays). Disabled when `trackId` falsy.
 *
 * #177 — `placeholderData: keepPreviousData` keeps the last successful
 * data visible while a background refetch is in flight. Without it,
 * `useTrackDetailQuery` would briefly surface `data: undefined` across
 * an `invalidateQueries`-driven refetch (the track-detail key is
 * invalidated by overlay.set / track.updated / card.* events on the
 * WS bus — see `app/eventBridge.tsx`). `TrackComponent` early-returns
 * `null` on `!detailQ.data`, which would unmount the entire track
 * subtree — including the lazy-loaded `XtermView` and its
 * `pendingThemeRef` / `sendRef`. On remount, those refs reset and
 * the very next `TerminalThemeUpdate` dispatch races the new WS
 * handshake (recoverable via the WS-queue + pendingThemeRef now, but
 * an unnecessary churn that masks the real "did the toggle reach the
 * daemon?" signal).
 *
 * Keeping previous data stops the unmount chain at the source:
 * refetches become transparent to children, XtermView stays mounted
 * across the overlay-update feedback loop on theme toggle, and the
 * dispatch hits a stable, OPEN WebSocket.
 *
 * Follow-up (separate issue): the CSS `[data-theme="dark"]` swap on
 * theme toggle triggers a layout change that RGL detects (a
 * dimension-affecting variable) → onLayoutChange → PATCH overlay →
 * overlay.set event → invalidate. That feedback loop is wasteful
 * and worth investigating once #177 is closed.
 */
export function useTrackDetailQuery(
  trackId: string | undefined | null,
  opts?: Partial<UseQueryOptions<KernelTrackDetail, Error>>,
) {
  return useQuery<KernelTrackDetail, Error>({
    ...trackDetailQueryOptions(trackId ?? ''),
    enabled: !!trackId,
    placeholderData: keepPreviousData,
    ...opts,
  });
}

export function useTrackBacklinksQuery(
  trackId: string | undefined | null,
  opts?: Partial<UseQueryOptions<api.TrackBacklinksResponse, Error>>,
) {
  return useQuery<api.TrackBacklinksResponse, Error>({
    ...trackBacklinksQueryOptions(trackId ?? ''),
    enabled: !!trackId,
    ...opts,
  });
}

export function useTrackFileList(
  trackId: string | undefined | null,
  path?: string | null,
  opts?: TrackFileQueryOpts,
) {
  return useQuery<api.TrackFsEntry[], Error>({
    queryKey: trackFileListQueryKey(trackId ?? '', path),
    queryFn: () => api.listTrackFiles(trackId ?? '', path),
    enabled: !!trackId && (opts?.enabled ?? true),
  });
}

export function useTrackFileContent(
  trackId: string | undefined | null,
  path: string | null | undefined,
  opts?: TrackFileQueryOpts,
) {
  return useQuery<api.TrackFsContent, Error>({
    queryKey: trackFileContentQueryKey(trackId ?? '', path),
    queryFn: () => api.catTrackFile(trackId ?? '', path ?? ''),
    enabled: !!trackId && !!path && (opts?.enabled ?? true),
  });
}

export function useTrackReportQuery(trackId: string | undefined | null) {
  return useQuery<api.TrackReportRead, Error>({
    ...trackReportQueryOptions(trackId ?? ''),
    enabled: !!trackId,
  });
}

/**
 * All overlays of a given entity kind (workspace-wide). Fed into adaptTrack
 * by IndexComponent so the Sidebar's status indicators reflect overlays on
 * tracks the user hasn't opened yet. eventBridge invalidates this on every
 * overlay.set / overlay.deleted so it stays current.
 */
export function useOverlaysByKindQuery(
  entity_kind: 'track' | 'card',
  opts?: Partial<UseQueryOptions<KernelOverlay[], Error>>,
) {
  return useQuery<KernelOverlay[], Error>({
    ...overlaysByKindQueryOptions(entity_kind),
    ...opts,
  });
}

/**
 * Calendar window — issue #250 PR 5. Returns every track overlapping
 * `[since, until]` (unix ms). `keepPreviousData` keeps the prior week's
 * grid visible while the next week's fetch is in flight so the navigation
 * arrows feel snappy. Invalidation lives in `eventBridge.tsx` —
 * `track.updated`, `track.lifecycle_changed`, and `track.deleted` all dirty
 * every cached window so the calendar redraws without a per-page refresh.
 */
export function useTracksRangeQuery(
  since: number,
  until: number,
  opts?: Partial<UseQueryOptions<KernelTrack[], Error>>,
) {
  return useQuery<KernelTrack[], Error>({
    ...tracksRangeQueryOptions(since, until),
    placeholderData: keepPreviousData,
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

export function useCreateAreaMutation() {
  const qc = useQueryClient();
  return useMutation<KernelArea, Error, NewAreaBody>({
    mutationFn: (body) => api.createArea(body),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.areas() });
    },
  });
}

/**
 * Update an area. Optimistic for `name` and `color` patches (the common
 * rename / palette-swap path). If the patch carries any other field
 * (currently only `sort`), we fall through to the plain invalidate-on-
 * settle path — reorder rollback for areas is rare and would require
 * snapshotting + replaying the full list re-sort.
 */
export function useUpdateAreaMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: AreaPatchBody };
  type Ctx = { previous: KernelArea[] | null };
  return useMutation<KernelArea, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateArea(id, body),
    onMutate: async ({ id, body }) => {
      const isOptimisticField =
        body.name !== undefined || body.color !== undefined;
      if (!isOptimisticField) return { previous: null };

      const key = queryKeys.areas();
      await qc.cancelQueries({ queryKey: key });
      const previous = qc.getQueryData<KernelArea[]>(key) ?? null;
      if (previous) {
        const now = Date.now();
        qc.setQueryData<KernelArea[]>(
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
        qc.setQueryData(queryKeys.areas(), context.previous);
      }
    },
    onSettled: () => {
      void qc.invalidateQueries({ queryKey: queryKeys.areas() });
    },
  });
}

export function useDeleteAreaMutation() {
  const qc = useQueryClient();
  return useMutation<void, Error, string>({
    mutationFn: (id) => api.deleteArea(id),
    onSuccess: (_v, id) => {
      void qc.invalidateQueries({ queryKey: queryKeys.areas() });
      // Drop the dead area's track list from cache.
      qc.removeQueries({ queryKey: queryKeys.tracksInArea(id) });
    },
  });
}

export function useCreateTrackMutation() {
  const qc = useQueryClient();
  return useMutation<KernelTrack, Error, NewTrackBody>({
    mutationFn: (body) => api.createTrack(body),
    onSuccess: (track) => {
      void qc.invalidateQueries({ queryKey: queryKeys.tracksInArea(track.area_id) });
    },
  });
}

/**
 * Update a track. Optimistic for `title` (rename) and `sort` (drag-reorder
 * within the area's track list). Other patch fields (e.g. `archived_at`)
 * stay non-optimistic — archive flips trigger cascading UI moves that are
 * cleaner to drive from the server-confirmed state.
 *
 * Two caches can hold a copy of the track: the list `['tracks', area_id]`
 * and the detail `['track', id]`. We update whichever ones are populated,
 * and snapshot both so onError can restore them.
 */
export function useUpdateTrackMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: TrackPatchBody };
  type Ctx = {
    previousList: { key: ReturnType<typeof queryKeys.tracksInArea>; value: KernelTrack[] } | null;
    previousDetail: KernelTrackDetail | null;
    detailKey: ReturnType<typeof queryKeys.trackDetail>;
  };
  return useMutation<KernelTrack, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateTrack(id, body),
    onMutate: async ({ id, body }) => {
      const detailKey = queryKeys.trackDetail(id);
      const empty: Ctx = { previousList: null, previousDetail: null, detailKey };
      const isOptimisticField =
        body.title !== undefined || body.sort !== undefined;
      if (!isOptimisticField) return empty;

      // Locate the track's area via cached detail first, then fall back to
      // scanning cached track lists. If neither cache is warm there's
      // nothing to optimistically mutate; we still let the request run.
      const cachedDetail = qc.getQueryData<KernelTrackDetail>(detailKey);
      let listKey: ReturnType<typeof queryKeys.tracksInArea> | null = null;
      if (cachedDetail) {
        listKey = queryKeys.tracksInArea(cachedDetail.track.area_id);
      } else {
        const all = qc.getQueriesData<KernelTrack[]>({ queryKey: ['tracks'] });
        for (const [k, v] of all) {
          if (v && v.some((w) => w.id === id)) {
            listKey = k as ReturnType<typeof queryKeys.tracksInArea>;
            break;
          }
        }
      }

      await qc.cancelQueries({ queryKey: detailKey });
      if (listKey) await qc.cancelQueries({ queryKey: listKey });

      const now = Date.now();
      const applyPatch = (w: KernelTrack): KernelTrack => ({
        ...w,
        ...(body.title != null ? { title: body.title } : {}),
        ...(body.sort != null ? { sort: body.sort } : {}),
        updated_at: now,
      });

      const ctx: Ctx = { ...empty };

      if (listKey) {
        const previousList = qc.getQueryData<KernelTrack[]>(listKey);
        if (previousList) {
          ctx.previousList = { key: listKey, value: previousList };
          qc.setQueryData<KernelTrack[]>(
            listKey,
            previousList.map((w) => (w.id === id ? applyPatch(w) : w)),
          );
        }
      }

      if (cachedDetail) {
        ctx.previousDetail = cachedDetail;
        qc.setQueryData<KernelTrackDetail>(detailKey, {
          ...cachedDetail,
          track: applyPatch(cachedDetail.track),
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
    onSettled: (track, _err, vars, context) => {
      // Prefer the server-confirmed area_id; fall back to whatever list
      // we touched optimistically. Either way we want the detail key
      // invalidated.
      const areaId = track?.area_id ?? context?.previousList?.value[0]?.area_id;
      if (areaId) {
        void qc.invalidateQueries({ queryKey: queryKeys.tracksInArea(areaId) });
      }
      void qc.invalidateQueries({ queryKey: queryKeys.trackDetail(vars.id) });
    },
  });
}

export function useDeleteTrackMutation() {
  const qc = useQueryClient();
  // We need the area id to invalidate the area's track list, so callers
  // pass `{ id, areaId }` — same shape the WS event would carry.
  return useMutation<void, Error, { id: string; areaId: string }>({
    mutationFn: ({ id }) => api.deleteTrack(id),
    onSuccess: (_v, { id, areaId }) => {
      void qc.invalidateQueries({ queryKey: queryKeys.tracksInArea(areaId) });
      qc.removeQueries({ queryKey: queryKeys.trackDetail(id) });
    },
  });
}

export function useCreateCardMutation() {
  const qc = useQueryClient();
  return useMutation<KernelCard, Error, { trackId: string; body: NewCardBody }>({
    mutationFn: ({ trackId, body }) => api.createCard(trackId, body),
    onSuccess: (card) => {
      void qc.invalidateQueries({ queryKey: queryKeys.trackDetail(card.track_id) });
    },
  });
}

/**
 * Update a card. Optimistic only for `sort` — the drag-reorder case
 * within a track's card grid. `payload` is intentionally NOT optimistic:
 * its shape is per-card-kind (see `cards/*` adapters) and a mid-edit
 * rollback would smear partial state across the card's bespoke UI.
 *
 * The caller doesn't pass `track_id` in vars, so we discover it by
 * scanning cached track details for the card. If we can't find it we
 * still send the mutation; onSettled then has no detail key to
 * invalidate and we rely on the WS `card.updated` fanout (see
 * `eventBridge.tsx`, which itself scans for the owning track).
 */
export function useUpdateCardMutation() {
  const qc = useQueryClient();
  type Vars = { id: string; body: CardPatchBody };
  type Ctx = {
    detailKey: ReturnType<typeof queryKeys.trackDetail> | null;
    previousDetail: KernelTrackDetail | null;
  };
  return useMutation<KernelCard, Error, Vars, Ctx>({
    mutationFn: ({ id, body }) => api.updateCard(id, body),
    onMutate: async ({ id, body }) => {
      const empty: Ctx = { detailKey: null, previousDetail: null };
      // Only `sort` is safe to optimistically mirror.
      if (body.sort === undefined || body.sort === null) return empty;

      const entries = qc.getQueriesData<KernelTrackDetail>({ queryKey: ['track'] });
      let detailKey: ReturnType<typeof queryKeys.trackDetail> | null = null;
      let previousDetail: KernelTrackDetail | null = null;
      for (const [k, v] of entries) {
        if (v && v.cards.some((c) => c.id === id)) {
          detailKey = k as ReturnType<typeof queryKeys.trackDetail>;
          previousDetail = v;
          break;
        }
      }
      if (!detailKey || !previousDetail) return empty;

      await qc.cancelQueries({ queryKey: detailKey });
      const now = Date.now();
      const nextSort = body.sort;
      qc.setQueryData<KernelTrackDetail>(detailKey, {
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
      const trackId = card?.track_id;
      if (trackId) {
        void qc.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
      } else if (context?.detailKey) {
        void qc.invalidateQueries({ queryKey: context.detailKey });
      }
    },
  });
}

export function useDeleteCardMutation() {
  const qc = useQueryClient();
  return useMutation<void, Error, { id: string; trackId: string }>({
    mutationFn: ({ id }) => api.deleteCard(id),
    onSuccess: (_v, { trackId }) => {
      void qc.invalidateQueries({ queryKey: queryKeys.trackDetail(trackId) });
    },
  });
}

// ---------------- settings ----------------
//
// Single global query + one mutation. Settings only feed the codex
// spawn path at the moment, so we don't bother with optimistic updates
// or WS-driven invalidation — the page just refetches after `PUT`.

export const settingsQueryOptions = () => ({
  queryKey: queryKeys.settings(),
  queryFn: () => api.getSettings(),
});

export function useSettingsQuery() {
  return useQuery<SettingsBag, Error>(settingsQueryOptions());
}

export function useUpdateSettingsMutation() {
  const qc = useQueryClient();
  return useMutation<SettingsBag, Error, SettingsPutBody>({
    mutationFn: (body) => api.putSettings(body),
    onSuccess: (bag) => {
      // The PUT response is the authoritative new bag — write it through
      // so the form re-primes without an extra round-trip.
      qc.setQueryData(queryKeys.settings(), bag);
    },
  });
}
