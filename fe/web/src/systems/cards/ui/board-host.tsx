import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, type Ref } from 'react';
import {
  GridLayout,
  useContainerWidth,
  type Layout,
  type LayoutItem,
  type ResizeHandleAxis,
} from 'react-grid-layout';

import { transformStrategy } from 'react-grid-layout/core';
import { MotionCell, motionPositionStyle, type Manipulation } from './motion-cell.tsx';
import { useState } from '../../../ui/state/public.ts';
import { FALLBACK_SIZE } from '../registry.js';
import type { CardHost } from '../host.js';
import { CardCell, type BoardHostItem } from './card-cell.tsx';
import {
  GRID_COLS,
  GRID_MARGIN,
  GRID_ROW_HEIGHT,
  layoutToPositions,
  reconcileLayout,
  type StoredPositions,
} from './layout.ts';

const EMPTY_POSITIONS: StoredPositions = Object.freeze({});
const MARGIN = Object.freeze([GRID_MARGIN, GRID_MARGIN] as const);
const PADDING = Object.freeze([0, 0] as const);
const RESIZE_HANDLES = Object.freeze(['se'] as const);
const DRAG_HANDLE = '.card-drag-handle';

/** Preserve the vendor handle classes/ref and expose the board-owned semantic locator. */
function resizeHandle(axis: ResizeHandleAxis, ref: Ref<HTMLElement>) {
  return <span ref={ref} className={`react-resizable-handle react-resizable-handle-${axis}`} data-nc-card-resize={axis} />;
}

type ScheduledLayoutUpdate =
  | Readonly<{ kind: 'scheduling' }>
  | Readonly<{ kind: 'animation-frame'; handle: number }>
  | Readonly<{ kind: 'timeout'; handle: ReturnType<typeof setTimeout> }>;

function revealWithinBoard(container: HTMLElement, active: HTMLElement): void {
  const containerBox = container.getBoundingClientRect();
  const activeBox = active.getBoundingClientRect();
  if (activeBox.top < containerBox.top) {
    container.scrollTop += activeBox.top - containerBox.top;
    return;
  }
  if (activeBox.bottom <= containerBox.bottom) return;
  container.scrollTop += activeBox.height > containerBox.height
    ? activeBox.top - containerBox.top
    : activeBox.bottom - containerBox.bottom;
}

export type { BoardHostItem } from './card-cell.tsx';

export function BoardHost({ host, items, activeCardId, visible, onRemoveCard }: {
  host: CardHost;
  items: readonly BoardHostItem[];
  activeCardId: string | null;
  visible: boolean;
  /** Puts a × on every deletable card's head; the caller owns the confirm and the mutation. */
  onRemoveCard?: (cardId: string) => void;
}) {
  const { width, containerRef, mounted } = useContainerWidth();
  const [manipulating, setManipulating] = useState<Manipulation>(null);
  const positionStrategy = useMemo(() => Object.freeze({ ...transformStrategy, calcStyle: motionPositionStyle }), []);
  const [stored, setStored] = useState<StoredPositions>(EMPTY_POSITIONS);
  const [boardHeight, setBoardHeight] = useState(0);
  const activeViewportHeight = items.some(item => item.card.id === activeCardId
    && host.registry.get(item.card.type)?.viewport === 'board') ? boardHeight : 0;

  const sized = useMemo(() => items.map((item) => ({
    id: item.card.id,
    size: host.registry.get(item.card.type)?.defaultSize ?? FALLBACK_SIZE,
  })), [host, items]);

  const layout = useMemo<LayoutItem[]>(
    () => reconcileLayout(sized, stored).map((placement) => ({
      i: placement.id,
      x: placement.x,
      y: placement.y,
      w: placement.w,
      h: placement.h,
      minW: placement.minW,
      minH: placement.minH,
    })),
    [sized, stored],
  );

  const pendingRef = useRef<Layout | null>(null);
  const scheduledRef = useRef<ScheduledLayoutUpdate | null>(null);
  const persistLayout = useCallback((next: Layout) => {
    pendingRef.current = next;
    if (scheduledRef.current !== null) return;

    const scheduling: ScheduledLayoutUpdate = { kind: 'scheduling' };
    scheduledRef.current = scheduling;
    const flush = () => {
      scheduledRef.current = null;
      const latched = pendingRef.current;
      pendingRef.current = null;
      if (latched === null) return;
      setStored(layoutToPositions(latched));
    };

    if (
      typeof requestAnimationFrame === 'function'
      && typeof cancelAnimationFrame === 'function'
    ) {
      const handle = requestAnimationFrame(flush);
      if (scheduledRef.current === scheduling) {
        scheduledRef.current = { kind: 'animation-frame', handle };
      }
      return;
    }

    const handle = setTimeout(flush, 0);
    if (scheduledRef.current === scheduling) {
      scheduledRef.current = { kind: 'timeout', handle };
    }
  }, []);

  useEffect(() => () => {
    const scheduled = scheduledRef.current;
    scheduledRef.current = null;
    pendingRef.current = null;
    if (scheduled === null || scheduled.kind === 'scheduling') return;
    if (scheduled.kind === 'animation-frame') {
      cancelAnimationFrame(scheduled.handle);
    } else {
      clearTimeout(scheduled.handle);
    }
  }, []);

  // Only the host knows the available viewport. Publish geometry without changing
  // RGL dimensions or involving individual renderers in ancestor scrolling.
  useLayoutEffect(() => {
    const container = containerRef.current;
    if (container === null) return;
    const measure = () => {
      if (container.clientHeight > 0) {
        container.style.setProperty('--nc-board-block-size', `${container.clientHeight}px`);
        setBoardHeight(container.clientHeight);
      }
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    return () => { observer.disconnect(); };
  }, [containerRef]);

  /* RGL applies its placement after the parent commit and persists a frame later, so an immediate scroll is overwritten; two frames lands after both. Only the board's own scrollport moves. */
  useLayoutEffect(() => {
    if (!visible || !mounted || activeCardId === null) return;
    const reveal = () => {
      const container = containerRef.current;
      const active = Array.from(
        container?.querySelectorAll<HTMLElement>('[data-nc-card-id]') ?? [],
      ).find((element) => element.dataset.ncCardId === activeCardId);
      if (container !== null && active !== undefined) revealWithinBoard(container, active);
    };
    if (
      typeof requestAnimationFrame !== 'function'
      || typeof cancelAnimationFrame !== 'function'
    ) {
      reveal();
      return;
    }
    let secondFrame: number | null = null;
    const firstFrame = requestAnimationFrame(() => {
      secondFrame = requestAnimationFrame(reveal);
    });
    return () => {
      cancelAnimationFrame(firstFrame);
      if (secondFrame !== null) cancelAnimationFrame(secondFrame);
    };
  }, [activeCardId, activeViewportHeight, containerRef, mounted, visible]);

  return (
    <div ref={containerRef} className="track-grid-wrap" data-nc-card-board="">
      {mounted && (
        <GridLayout
          className="track-grid"
          width={width}
          layout={layout}
          gridConfig={{
            cols: GRID_COLS,
            rowHeight: GRID_ROW_HEIGHT,
            margin: MARGIN,
            containerPadding: PADDING,
          }}
          dragConfig={{ handle: DRAG_HANDLE }}
          resizeConfig={{ handles: RESIZE_HANDLES, handleComponent: resizeHandle }}
          positionStrategy={positionStrategy}
          onDragStart={() => { setManipulating('drag'); }}
          onDragStop={() => { setManipulating(null); }}
          onResizeStart={() => { setManipulating('resize'); }}
          onResizeStop={() => { setManipulating(null); }}
          onLayoutChange={persistLayout}
        >
          {items.map((item) => (
            <MotionCell
              enabled={visible}
              manipulation={manipulating}
              key={item.card.id}
              className="track-card"
              data-nc-card-cell=""
              data-nc-card-id={item.card.id}
              data-nc-card-viewport={host.registry.get(item.card.type)?.viewport}
            >
              <CardCell
                host={host}
                item={item}
                focused={visible && item.card.id === activeCardId}
                visible={visible}
                onRemove={onRemoveCard === undefined || item.deletable === false
                  ? undefined
                  : () => onRemoveCard(item.card.id)}
              />
            </MotionCell>
          ))}
        </GridLayout>
      )}
    </div>
  );
}
