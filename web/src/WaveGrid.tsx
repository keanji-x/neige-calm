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
import { useOverlayState } from './hooks/useOverlayState';
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

  const storedPositions = layoutValue.positions;

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
            <div key={slotKey(slot, i)} className="wave-card">
              {slot.kind === 'card' ? (
                <WaveCard
                  card={slot.card}
                  onClose={() => onRemoveCard(i)}
                />
              ) : (
                <UnknownCard
                  kernelKind={slot.kernelKind}
                  onClose={() => onRemoveCard(i)}
                />
              )}
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
