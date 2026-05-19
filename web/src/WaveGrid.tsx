import { useEffect, useMemo, useState } from 'react';
import {
  GridLayout,
  useContainerWidth,
  type Layout,
  type LayoutItem,
} from 'react-grid-layout';
import 'react-grid-layout/css/styles.css';
import 'react-resizable/css/styles.css';

import { WaveCard } from './ui';
import { sizeFor } from './cards/registry';
import type { WaveCardData } from './types';

const COLS = 12;
const ROW_HEIGHT = 40;
const MARGIN: readonly [number, number] = [14, 14];

// Card identity: kernel id if we have it, otherwise positional fallback.
// Stable across reorders so RGL keys + persisted layout don't drift.
function cardKey(card: WaveCardData, idx: number): string {
  return card.id ?? `idx-${idx}`;
}

const STORAGE_PREFIX = 'calm:layout:';

interface StoredEntry {
  x: number;
  y: number;
  w: number;
  h: number;
}

function loadStored(waveId: string): Record<string, StoredEntry> {
  try {
    const raw = localStorage.getItem(STORAGE_PREFIX + waveId);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object') return {};
    return parsed;
  } catch {
    return {};
  }
}

function saveStored(waveId: string, layout: Layout) {
  try {
    const out: Record<string, StoredEntry> = {};
    for (const it of layout) {
      out[it.i] = { x: it.x, y: it.y, w: it.w, h: it.h };
    }
    localStorage.setItem(STORAGE_PREFIX + waveId, JSON.stringify(out));
  } catch {
    /* quota or private-mode — silently drop, layout falls back to default next mount */
  }
}

// Build a complete LayoutItem[] for the current card list: reuse stored
// positions where we have them, auto-pack newcomers in row-major order at
// the bottom.
function reconcile(
  cards: WaveCardData[],
  stored: Record<string, StoredEntry>,
): LayoutItem[] {
  // The lowest free row, computed from stored entries we plan to keep.
  let nextY = 0;
  for (let i = 0; i < cards.length; i++) {
    const key = cardKey(cards[i], i);
    const e = stored[key];
    if (e) nextY = Math.max(nextY, e.y + e.h);
  }
  let cursorX = 0;
  let rowH = 0;
  const result: LayoutItem[] = [];
  for (let i = 0; i < cards.length; i++) {
    const card = cards[i];
    const key = cardKey(card, i);
    const size = sizeFor(card);
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

export function WaveGrid({
  waveId,
  cards,
  onRemoveCard,
}: {
  waveId: string;
  cards: WaveCardData[];
  onRemoveCard: (idx: number) => void;
}) {
  const { width, containerRef, mounted } = useContainerWidth();

  // Layout key set — recompute when cards arrive/leave or the wave changes.
  // In-place layout edits are persisted via onLayoutChange and don't need to
  // round-trip through state.
  const cardKeys = useMemo(
    () => cards.map((c, i) => cardKey(c, i)).join('|'),
    [cards],
  );
  const [layout, setLayout] = useState<LayoutItem[]>(() =>
    reconcile(cards, loadStored(waveId)),
  );
  useEffect(() => {
    setLayout(reconcile(cards, loadStored(waveId)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [waveId, cardKeys]);

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
            // RGL hands us a readonly Layout; clone so our state stays
            // mutable (set/spread later) without TS yelling.
            const snapshot = next.slice();
            setLayout(snapshot);
            saveStored(waveId, next);
          }}
        >
          {cards.map((c, i) => (
            <div key={cardKey(c, i)} className="card-slot">
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
              <WaveCard card={c} />
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
