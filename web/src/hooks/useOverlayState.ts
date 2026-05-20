// useOverlayState — the synced replacement for `useState`.
//
// **The first feature riding on the full sync engine** (design doc §4.1).
// Components reach for this whenever the state they want to hold belongs on
// the kernel: it lands in the `overlays` table, broadcasts on the bus,
// replays on reconnect, persists to IndexedDB via the TanStack Query cache,
// and is type-branded `Persistent<T>` so a future drift into `useState`
// fails to compile (see `web/src/shared/state.ts`).
//
// Why it exists in this shape
// ---------------------------
//
// `useState(initial)` returns `[value, setValue]`. So does `useOverlayState`.
// At the call site the two are intentionally interchangeable — the only
// difference is *where the truth lives*. `useState` is component memory and
// dies on remount; `useOverlayState` is `POST /api/overlays`, an event log
// row, an `overlay.set` WS broadcast, and a Query cache entry. The call
// site keeps the same destructuring shape so adoption is a one-line swap.
//
// Internal flow
// -------------
//
//   1. `useQuery({ queryKey: ['overlay', plugin_id, entity_kind, entity_id, kind] })`
//      — fetches via `GET /api/overlays?entity_kind=...&entity_id=...`,
//      filters the response to the requested `kind`. The `eventBridge`
//      invalidates this query family on `overlay.set` / `overlay.deleted`
//      (see `web/src/app/eventBridge.tsx`), so live updates from other
//      clients land here automatically.
//   2. `useMutation` wrapping `POST /api/overlays` (upsert). The setter
//      runs an optimistic update against the queryKey above and rolls back
//      on error — same shape as `useUpdateCoveMutation` in `api/queries.ts`.
//   3. The functional setter form `(prev) => next` peeks at the current
//      cached value via `qc.getQueryData` to compute the next.
//
// Pending state surfaces as the `default` value — the hook deliberately
// does NOT return a `loading` boolean. The call site looks like `useState`,
// no exceptions; the cost is a single render with `default` before the
// query resolves, which IndexedDB rehydration (Scope F) eliminates on
// warm reloads.
//
// Errors
// ------
//
// Mutation errors roll back the optimistic value and console.error — no
// toast surface yet. A future Scope plumbs these into a notification UI.

import { useCallback } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import * as api from '../api/calm';
import type { Persistent } from '../shared/state';
import type { KernelOverlay, NewOverlayBody } from '../api/wire';

/** Default plugin id for app-level (kernel-owned) state. Plugin-defined
 *  overlay kinds pass their own `pluginId`. */
const KERNEL_PLUGIN_ID = 'kernel';

/** Permitted entity kinds. Matches the union on `NewOverlayBody.entity_kind`.
 *  Kept local so callers don't have to import it; `entity_kind` accepts the
 *  same shape via the public string type alias below. */
export type OverlayEntityKind = 'wave' | 'card' | 'view';

export interface UseOverlayStateOptions<T> {
  /** Kernel entity that owns this overlay. Today: `'view'` for app-level
   *  state (e.g. WaveGrid layout), `'wave'` / `'card'` for content-attached
   *  overlays. */
  entity_kind: OverlayEntityKind;
  /** Stable id of the owning entity — `waveId`, `cardId`, etc. */
  entity_id: string;
  /** Overlay kind. Matched against `validate_overlay_payload` on the
   *  server: `'layout'`, `'status'`, `'progress'`, … (plugin-defined kinds
   *  pass through opaquely). */
  kind: string;
  /** Value returned before the query resolves (and when no overlay row
   *  exists yet). Mirrors `useState`'s initial-value semantics. */
  default: T;
  /** Plugin attribution. Defaults to `"kernel"` for app-level state. */
  pluginId?: string;
}

/** Tuple shape — by design identical to `useState`'s, except for the
 *  brand on the value. The setter accepts either a value or `(prev) => next`. */
export type UseOverlayStateReturn<T> = readonly [
  Persistent<T>,
  (next: T | ((prev: T) => T)) => void,
];

/**
 * The kernel-synced equivalent of `useState`. See file header for the
 * full rationale.
 *
 * @example
 * const [layout, setLayout] = useOverlayState({
 *   entity_kind: 'view',
 *   entity_id: waveId,
 *   kind: 'layout',
 *   default: { positions: {} },
 * });
 */
export function useOverlayState<T>(
  opts: UseOverlayStateOptions<T>,
): UseOverlayStateReturn<T> {
  const { entity_kind, entity_id, kind } = opts;
  const pluginId = opts.pluginId ?? KERNEL_PLUGIN_ID;
  // Refresh-anchored stable default reference. Identity may matter when
  // callers wrap the tuple in dependency lists; we re-read on every render
  // since `opts.default` is the caller's responsibility to stabilize.
  const defaultValue = opts.default;
  const qc = useQueryClient();

  const queryKey = overlayStateQueryKey(pluginId, entity_kind, entity_id, kind);

  const query = useQuery<T, Error>({
    queryKey,
    // Disable the auto-fetch when entity_id is empty — mirrors the pattern
    // in `useWaveDetailQuery`. A loader / parent should always supply a
    // real id, but defensive: empty id => return default without hitting
    // the network.
    enabled: entity_id.length > 0,
    queryFn: async () => {
      const overlays = await api.listOverlays(entity_kind, entity_id);
      const match = overlays.find(
        (o) => o.plugin_id === pluginId && o.kind === kind,
      );
      // No matching row yet — return the default. The next setter call
      // upserts and populates the cache.
      if (!match) return defaultValue;
      return match.payload as T;
    },
  });

  // The mutation context carries the rollback snapshot we captured
  // *synchronously* in the setter (before `mutate()` was called). Stuffing
  // it through the variables is awkward but cleaner than a closure-over-ref
  // smuggle: we get React Query's onError/onSuccess wiring for free, and
  // the per-call rollback is structurally local to the call site.
  type Vars = { value: T; previous: T | undefined };

  const mutation = useMutation<KernelOverlay, Error, Vars>({
    mutationFn: async ({ value }) => {
      const body: NewOverlayBody = {
        plugin_id: pluginId,
        entity_kind,
        entity_id,
        kind,
        payload: value,
      };
      return api.upsertOverlay(body);
    },
    onError: (err, vars) => {
      // Roll back. The query was optimistically advanced in the setter
      // (synchronously, before this mutation ran); revert now so the UI
      // doesn't keep showing a write that never made it.
      qc.setQueryData<T | undefined>(queryKey, vars.previous);
      // Surface to console for diagnostics. A real toast / notification
      // path is a follow-up; this matches today's other mutation hooks.
      // eslint-disable-next-line no-console
      console.error('useOverlayState: upsert failed', {
        entity_kind,
        entity_id,
        kind,
        err,
      });
    },
    onSuccess: (overlay) => {
      // Write the server-confirmed payload through. The co-mounted
      // `eventBridge` `overlay.set` invalidate will arrive moments later
      // and refetch; setting it here means even if the WS event is
      // briefly delayed we still settle on the canonical value without
      // an extra render of the optimistic snapshot.
      qc.setQueryData<T>(queryKey, overlay.payload as T);
    },
  });

  const setter = useCallback(
    (next: T | ((prev: T) => T)) => {
      // **Optimistic update happens here, synchronously**, so a caller
      // that re-renders immediately after `setLayout(x)` sees `x` on the
      // very next render — true `useState` parity. RQ's `onMutate` runs
      // *after* an `await cancelQueries`, which is one microtask too
      // late for the synchronous-visibility guarantee.
      const prev = qc.getQueryData<T>(queryKey) ?? defaultValue;
      const value =
        typeof next === 'function'
          ? (next as (p: T) => T)(prev)
          : next;
      // Cancel any in-flight refetch FIRST so a late-arriving GET
      // response can't trample our optimistic write. Fire-and-forget;
      // the cancel races the optimistic setQueryData but neither order
      // produces a wrong value — setQueryData wins by recency, and
      // cancelQueries doesn't drop data, only in-flight fetches.
      void qc.cancelQueries({ queryKey });
      qc.setQueryData<T>(queryKey, value);
      mutation.mutate({ value, previous: prev });
    },
    // `mutation.mutate` is referentially stable per RQ contract; `qc` and
    // `queryKey` content are the load-bearing deps. We list the key
    // pieces individually so a parent re-rendering doesn't churn this
    // identity unless the addressed overlay actually changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [qc, pluginId, entity_kind, entity_id, kind, defaultValue, mutation.mutate],
  );

  // Brand the returned value as `Persistent<T>`. Runtime: identity — the
  // brand is a type-system phantom (see `web/src/shared/state.ts`). The
  // shadowed `useState` in `shared/state.ts` produces `never` when handed
  // a `Persistent<T>`; the ESLint rule + the type error together close
  // the "accidentally store this in local state" gap.
  const value = (query.data ?? defaultValue) as Persistent<T>;

  return [value, setter] as const;
}

/**
 * Public query-key factory. Importable by `eventBridge`, tests, or
 * anywhere that wants to peek / write through the same cache slot the
 * hook reads from. Kept here (next to the only canonical consumer)
 * rather than in `api/queries.ts` so the hook stays self-contained.
 *
 * The shape (`['overlay', plugin_id, entity_kind, entity_id, kind]`) is
 * what `persistConfig.isPersistableQueryKey` matches via the
 * `key.length >= 2` rule under `'overlay'` — so cached overlay state
 * survives reloads via IndexedDB (Scope F).
 */
export function overlayStateQueryKey(
  plugin_id: string,
  entity_kind: string,
  entity_id: string,
  kind: string,
): readonly [string, string, string, string, string] {
  return ['overlay', plugin_id, entity_kind, entity_id, kind] as const;
}
