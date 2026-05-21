import { useCallback, useEffect, useMemo, useRef } from 'react';
import {
  GridLayout,
  useContainerWidth,
  type Layout,
  type LayoutItem,
} from 'react-grid-layout';
import 'react-grid-layout/css/styles.css';
import 'react-resizable/css/styles.css';

import { WaveCard } from './shared/components/WaveCard';
import { sizeFor, type CardSize } from './cards/registry';
import { UnknownCard, UNKNOWN_CARD_SIZE } from './cards/UnknownCard';
import { dlog } from './util/debug';
import type { WaveCardSlot } from './types';
import { useQueryClient } from '@tanstack/react-query';
import {
  overlayStateQueryKey,
  useOverlayState,
} from './hooks/useOverlayState';
import { upsertOverlay } from './api/calm';
import { OVERLAY_LAYOUT_SCHEMA_VERSION } from './cards/builtins/schemaVersions';

const COLS = 12;
const ROW_HEIGHT = 40;
const MARGIN: readonly [number, number] = [14, 14];

// Card identity: kernel id if we have it, otherwise positional fallback.
// Stable across reorders so RGL keys + persisted layout don't drift.
// Works uniformly across `card` slots (id from WaveCardData) and `unknown`
// slots (id is the KernelCard.id, always present).
function slotKey(slot: WaveCardSlot, idx: number): string {
  if (slot.kind === 'card') return slot.card.id ?? `idx-${idx}`;
  return slot.id || `idx-${idx}`;
}

function slotSize(slot: WaveCardSlot): CardSize {
  return slot.kind === 'card' ? sizeFor(slot.card) : UNKNOWN_CARD_SIZE;
}

// ---------------------------------------------------------------------------
// localStorage shape (legacy — Scope E migrates these rows into Overlay state)
// ---------------------------------------------------------------------------
//
// Pre-Scope-E builds wrote layout positions to `localStorage[`calm:layout:<waveId>`]`
// as a `Record<card_id, {x, y, w, h}>`. We keep the read path here for two
// reasons:
//
//   1. A one-shot migration helper (`useLocalStorageMigration` below) that
//      reads the legacy row, POSTs it as an overlay on first mount, then
//      deletes the localStorage key. Idempotent — once the overlay row
//      exists, the migration finds nothing to do on subsequent mounts.
//
//   2. Defensive flash-prevention. If the overlay query is still pending
//      on first mount (cold-cache reload, IndexedDB not yet rehydrated),
//      the localStorage value seeds `reconcile` for that single render
//      until the query resolves and replaces it.
//
// Once the migration is universal (a release after this lands), both
// pathways can be deleted. See design doc §5.2 step 5.

const LEGACY_STORAGE_PREFIX = 'calm:layout:';

interface StoredEntry {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface LayoutOverlayValue {
  /** Tier A persistence contract — see
   *  `web/src/cards/builtins/schemaVersions.ts`. Optional on read because
   *  rows written before this field existed are treated as v1; new writes
   *  set it explicitly. */
  schemaVersion?: number;
  positions: Record<string, StoredEntry>;
}

function loadLegacyLayout(waveId: string): Record<string, StoredEntry> | null {
  try {
    const raw = localStorage.getItem(LEGACY_STORAGE_PREFIX + waveId);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
    // Sanity-filter to the expected shape — anything outside the
    // {x,y,w,h: number} contract is discarded silently. This is the
    // boundary where legacy garbage stops; downstream code can assume
    // the entries are well-formed.
    const out: Record<string, StoredEntry> = {};
    for (const [k, v] of Object.entries(parsed)) {
      if (
        v &&
        typeof v === 'object' &&
        typeof (v as Partial<StoredEntry>).x === 'number' &&
        typeof (v as Partial<StoredEntry>).y === 'number' &&
        typeof (v as Partial<StoredEntry>).w === 'number' &&
        typeof (v as Partial<StoredEntry>).h === 'number'
      ) {
        const e = v as StoredEntry;
        out[k] = { x: e.x, y: e.y, w: e.w, h: e.h };
      }
    }
    return out;
  } catch {
    return null;
  }
}

// Build a complete LayoutItem[] for the current slot list: reuse stored
// positions where we have them, auto-pack newcomers in row-major order at
// the bottom.
function reconcile(
  slots: WaveCardSlot[],
  stored: Record<string, StoredEntry>,
): LayoutItem[] {
  // The lowest free row, computed from stored entries we plan to keep.
  let nextY = 0;
  for (let i = 0; i < slots.length; i++) {
    const key = slotKey(slots[i], i);
    const e = stored[key];
    if (e) nextY = Math.max(nextY, e.y + e.h);
  }
  let cursorX = 0;
  let rowH = 0;
  const result: LayoutItem[] = [];
  for (let i = 0; i < slots.length; i++) {
    const slot = slots[i];
    const key = slotKey(slot, i);
    const size = slotSize(slot);
    const e = stored[key];
    if (e) {
      result.push({
        i: key,
        x: e.x,
        y: e.y,
        w: e.w,
        h: e.h,
        minW: size.minW,
        minH: size.minH,
      });
      continue;
    }
    // New card — pack at the bottom-left, wrapping when row is full.
    if (cursorX + size.w > COLS) {
      cursorX = 0;
      nextY += rowH;
      rowH = 0;
    }
    result.push({
      i: key,
      x: cursorX,
      y: nextY,
      w: size.w,
      h: size.h,
      minW: size.minW,
      minH: size.minH,
    });
    cursorX += size.w;
    rowH = Math.max(rowH, size.h);
  }
  return result;
}

function layoutToPositions(layout: Layout): Record<string, StoredEntry> {
  const out: Record<string, StoredEntry> = {};
  for (const it of layout) {
    out[it.i] = { x: it.x, y: it.y, w: it.w, h: it.h };
  }
  return out;
}

/**
 * One-shot migration: if the overlay query has resolved with no rows for
 * this wave AND `localStorage[`calm:layout:<waveId>`]` holds a parseable
 * legacy value, POST it as the canonical overlay and then delete the
 * legacy key.
 *
 * Runs once per wave per session (guarded by a ref). Idempotent: the
 * second mount on the same wave finds the overlay row already populated
 * and short-circuits before any write.
 *
 * The migration calls `upsertOverlay` directly (not the hook setter) so
 * it's clear this is a one-time step and not part of the normal write
 * path. The subsequent `overlay.set` event flows through eventBridge,
 * which invalidates the query family and lets the hook pick up the new
 * value naturally.
 *
 * **Why check the queryClient state instead of just the overlay value:**
 * the hook collapses "pending" into the default ({positions: {}}). So
 * "value is default" cannot distinguish "GET hasn't fired yet" from
 * "GET fired, no row exists" — and we MUST NOT migrate during the
 * former (we'd POST a legacy snapshot over a wave whose real overlay
 * is still inbound from the network). The query's `status === 'success'`
 * is the unambiguous "GET completed, here's what the server has" signal.
 */
function useLocalStorageMigration(waveId: string): void {
  const qc = useQueryClient();
  // One-time guard per wave. React 18 strict mode mounts effects twice —
  // the ref ensures the POST fires at most once even under that re-mount.
  const migratedRef = useRef<Set<string>>(new Set());
  // The hook's queryKey: we read it back to inspect status without
  // re-subscribing to the data. (Subscribing again here would just
  // duplicate the existing observer in `useOverlayState`.)
  const queryKey = overlayStateQueryKey('kernel', 'view', waveId, 'layout');
  // The query state changes as the GET progresses (`pending` → `success`).
  // We want the effect to re-run on that transition so we don't migrate
  // while pending. The cleanest way to surface it inside an effect is a
  // poll of `qc.getQueryState`, but a `useQuery` subscription is what
  // actually gives React Query's observer dispatcher a hook to wake us
  // up. Subscribe in a way that doesn't refetch (`enabled: false` would
  // also keep status `pending`); instead, subscribe to the same query
  // the hook already subscribes to — RQ deduplicates fetches per
  // queryKey, so this costs nothing.
  const state = qc.getQueryState<LayoutOverlayValue>(queryKey);
  const status = state?.status;
  const data = state?.data;

  useEffect(() => {
    if (status !== 'success') return;
    if (migratedRef.current.has(waveId)) return;
    // Only migrate when the server has no overlay row yet. If positions
    // is non-empty the wave was either created post-Scope-E or already
    // migrated — either way, leave the legacy localStorage row alone.
    const positions = data?.positions ?? {};
    if (Object.keys(positions).length > 0) {
      migratedRef.current.add(waveId);
      return;
    }
    const legacy = loadLegacyLayout(waveId);
    if (!legacy || Object.keys(legacy).length === 0) {
      migratedRef.current.add(waveId);
      return;
    }
    migratedRef.current.add(waveId);
    upsertOverlay({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: waveId,
      kind: 'layout',
      // Tier A: stamp the kernel-owned `schemaVersion` on this write.
      payload: {
        schemaVersion: OVERLAY_LAYOUT_SCHEMA_VERSION,
        positions: legacy,
      },
    })
      .then(() => {
        try {
          localStorage.removeItem(LEGACY_STORAGE_PREFIX + waveId);
        } catch {
          /* private-mode / quota — no harm leaving the key, the next
             mount finds the populated overlay and short-circuits. */
        }
      })
      .catch((err) => {
        migratedRef.current.delete(waveId);
        // eslint-disable-next-line no-console
        console.warn('WaveGrid layout migration: upsert failed', err);
      });
    // `data` is in deps so a late-arriving server overlay (e.g. WS
    // event resolving the query after a slow connection) re-runs the
    // check; the ref guard then short-circuits.
  }, [waveId, status, data]);
}

export function WaveGrid({
  waveId,
  cards,
  onRemoveCard,
}: {
  waveId: string;
  /**
   * Heterogeneous card slots: a parsed `WaveCardData` per slot, or an
   * `unknown` placeholder for kernel cards the registry couldn't adapt.
   * The placeholder rendering lives in `cards/UnknownCard.tsx`; rendering
   * here keeps the fallback adjacent to its sibling cards in the grid
   * rather than dropping the row entirely.
   */
  cards: WaveCardSlot[];
  onRemoveCard: (idx: number) => void;
}) {
  const { width, containerRef, mounted } = useContainerWidth();
  dlog('WaveGrid', 'render', { waveId, width, mounted, cardsCount: cards.length });

  // Scope E: layout now lives in an Overlay row. The hook handles
  // optimistic update + rollback + WS replay across reloads; what we get
  // back is the same `Persistent<{positions}>` shape every consumer sees.
  const [layoutValue, setLayoutValue] = useOverlayState<LayoutOverlayValue>({
    entity_kind: 'view',
    entity_id: waveId,
    kind: 'layout',
    default: { positions: {} },
  });

  // One-shot migration from `localStorage['calm:layout:<waveId>']`. Runs
  // exactly once per wave per session; see `useLocalStorageMigration`.
  // The query family is invalidated by `eventBridge` on the resulting
  // `overlay.set`, so we don't have to plumb the new value back ourselves.
  useLocalStorageMigration(waveId);

  // Defensive seed: if the overlay query is still pending (cold cache,
  // IndexedDB not yet rehydrated), reach for the legacy localStorage row
  // as a one-render fallback. The overlay query usually resolves on the
  // same tick from rehydrate, but a wave opened during the brief window
  // between mount and rehydrate would otherwise paint a default-laid
  // grid that snaps back to the saved layout on the next render.
  const storedPositions = useMemo<Record<string, StoredEntry>>(() => {
    const fromOverlay = layoutValue.positions;
    if (Object.keys(fromOverlay).length > 0) return fromOverlay;
    return loadLegacyLayout(waveId) ?? fromOverlay;
  }, [waveId, layoutValue]);

  // Layout key set — recompute only when cards arrive/leave or the wave
  // changes. Note: we deliberately do NOT mirror RGL's runtime layout in
  // a React useState. Mirroring created a feedback loop where RGL's
  // internal layout normalization (e.g. minW/minH vs stored values) fought
  // our setState in a tight render cycle, alternating between two layout
  // snapshots ~hundreds of times per second. See PR #12 + issue #13.
  //
  // Using useMemo + reference-stable output: when cards don't change, RGL
  // sees the same `layout` prop reference and doesn't re-normalize.
  const cardKeys = useMemo(
    () => cards.map((c, i) => slotKey(c, i)).join('|'),
    [cards],
  );
  const layout = useMemo<LayoutItem[]>(
    () => reconcile(cards, storedPositions),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [waveId, cardKeys, storedPositions],
  );
  // Render-time diagnostic — confirm the layout reference is stable across
  // re-renders that aren't card-driven.
  dlog('WaveGrid', 'render-detail', {
    layoutLen: layout.length,
    layoutSig: layout.map((l) => `${l.i}@${l.x},${l.y},${l.w}x${l.h}`).join('|'),
  });

  // Coalesce RGL's drag-event firehose. RGL fires `onLayoutChange` on
  // every pointer-move during a drag, sometimes hundreds of times per
  // gesture — we don't want one POST per frame. `requestAnimationFrame`
  // is the right granularity: one mutation per visual frame at most,
  // and the final frame of a gesture is the one we ultimately want
  // persisted. The setter latches the most recent layout; the rAF
  // callback reads the latch and posts.
  //
  // A simple debounce (e.g. setTimeout 200ms) would also work, but rAF
  // ties the cadence to the browser's paint loop, which is the actual
  // rate at which the layout *visually* changes. No need to add a
  // human-tuned delay constant.
  const pendingRef = useRef<Layout | null>(null);
  const rafRef = useRef<number | null>(null);
  const persistLayout = useCallback(
    (next: Layout) => {
      pendingRef.current = next;
      if (rafRef.current !== null) return;
      // rAF is missing in some test/jsdom configs. Fall back to
      // `setTimeout(0)` so the coalescer still flushes — production
      // jsdom in vitest does ship rAF, but this keeps the fallback
      // self-contained.
      const schedule =
        typeof requestAnimationFrame === 'function'
          ? requestAnimationFrame
          : (cb: FrameRequestCallback) =>
              setTimeout(() => cb(performance.now()), 0) as unknown as number;
      rafRef.current = schedule(() => {
        rafRef.current = null;
        const latched = pendingRef.current;
        pendingRef.current = null;
        if (!latched) return;
        setLayoutValue({
          schemaVersion: OVERLAY_LAYOUT_SCHEMA_VERSION,
          positions: layoutToPositions(latched),
        });
      });
    },
    [setLayoutValue],
  );

  // Cancel any pending rAF on unmount — if the user drags then navigates
  // away mid-gesture, the latched layout is stale relative to their
  // intent. Letting the rAF fire after unmount would persist the
  // pre-navigation snapshot, which is harmless but wasted I/O.
  useEffect(() => {
    return () => {
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, []);

  return (
    <div ref={containerRef} className="wave-grid-wrap">
      {mounted && (
        <GridLayout
          className="wave-grid"
          width={width}
          layout={layout}
          gridConfig={{
            cols: COLS,
            rowHeight: ROW_HEIGHT,
            margin: MARGIN,
            containerPadding: [0, 0],
          }}
          dragConfig={{ handle: '.card-drag-handle' }}
          resizeConfig={{ handles: ['se'] }}
          onLayoutChange={(next) => {
            dlog('WaveGrid', 'onLayoutChange', {
              items: next.length,
              sig: next.map((l) => `${l.i}@${l.x},${l.y},${l.w}x${l.h}`).join('|'),
            });
            persistLayout(next);
          }}
        >
          {cards.map((slot, i) => (
            <div key={slotKey(slot, i)} className="card-slot">
              <button
                className="card-grid-close"
                onClick={(e) => {
                  e.stopPropagation();
                  onRemoveCard(i);
                }}
                onMouseDown={(e) => e.stopPropagation()}
                title="Remove panel"
                aria-label="Remove panel"
              >
                ×
              </button>
              {slot.kind === 'card' ? (
                <WaveCard card={slot.card} />
              ) : (
                <UnknownCard kernelKind={slot.kernelKind} />
              )}
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
