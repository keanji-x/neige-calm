import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, type ComponentType } from 'react';
import {
  GridLayout,
  useContainerWidth,
  type Layout,
  type LayoutItem,
} from 'react-grid-layout';

import { useState } from '../../../ui/state/public.ts';
import { FALLBACK_SIZE, type CardComponentProps, type RegisteredCard } from '../registry.js';
import type { CardHost, CardHostCapabilities, MountedCard } from '../host.js';
import { CardHead } from './card-head.tsx';
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

export type BoardHostItem = Readonly<{
  card: RegisteredCard;
  title: string;
  originalIndex: number;
  /**
   * The kernel's `deletable` bit, carried through unchanged. `false` is the
   * kernel saying it owns this row, and a card head that offered a × the kernel
   * would refuse is worse than one that offers nothing — the refusal would
   * arrive as an error on a gesture the UI had already promised.
   *
   * Optional, and absent means deletable: a wire payload from a pre-#229 server
   * omits the field, and the same "undefined is user-deletable" reading is what
   * `cardWireSchema`'s `.default(true)` already encodes.
   */
  deletable?: boolean;
}>;

function cardWithTitle(item: BoardHostItem): RegisteredCard {
  const card = item.card;
  if (!('title' in card)) return card;
  const current = (card as { title: string | null }).title;
  if (current !== null && current !== '') return card;
  return { ...card, title: item.title };
}

export function BoardHost({ host, items, activeCardId, visible, onRemoveCard }: {
  host: CardHost;
  items: readonly BoardHostItem[];
  activeCardId: string | null;
  visible: boolean;
  /**
   * Supplying this puts a × on the head of every deletable card. The board does
   * not delete anything itself — the caller owns the confirm and the mutation,
   * exactly as the CARDS panel's row does, so both gestures land on one dialog.
   */
  onRemoveCard?: (cardId: string) => void;
}) {
  const { width, containerRef, mounted } = useContainerWidth();
  const [stored, setStored] = useState<StoredPositions>(EMPTY_POSITIONS);

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
  const rafRef = useRef<number | null>(null);
  const persistLayout = useCallback((next: Layout) => {
    pendingRef.current = next;
    if (rafRef.current !== null) return;
    const schedule = typeof requestAnimationFrame === 'function'
      ? requestAnimationFrame
      : (cb: FrameRequestCallback) => setTimeout(() => cb(performance.now()), 0) as unknown as number;
    rafRef.current = schedule(() => {
      rafRef.current = null;
      const latched = pendingRef.current;
      pendingRef.current = null;
      if (latched === null) return;
      setStored(layoutToPositions(latched));
    });
  }, []);

  useEffect(() => () => {
    if (rafRef.current === null) return;
    cancelAnimationFrame(rafRef.current);
    rafRef.current = null;
  }, []);

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
          resizeConfig={{ handles: RESIZE_HANDLES }}
          onLayoutChange={persistLayout}
        >
          {items.map((item) => (
            <div
              key={item.card.id}
              className="track-card"
              data-nc-card-cell=""
              data-nc-card-id={item.card.id}
            >
              <BoardCell
                host={host}
                item={item}
                focused={visible && item.card.id === activeCardId}
                visible={visible}
                onRemove={onRemoveCard === undefined || item.deletable === false
                  ? undefined
                  : () => onRemoveCard(item.card.id)}
              />
            </div>
          ))}
        </GridLayout>
      )}
    </div>
  );
}

function BoardCell({ host, item, focused, visible, onRemove }: {
  host: CardHost;
  item: BoardHostItem;
  focused: boolean;
  visible: boolean;
  onRemove?: () => void;
}) {
  const mountedRef = useRef<MountedCard | null>(null);
  const cardRef = useRef(item.card);
  cardRef.current = cardWithTitle(item);
  const [capabilities, setCapabilities] = useState<CardHostCapabilities | null>(null);
  const entry = host.registry.get(item.card.type);
  const Component: ComponentType<CardComponentProps> | undefined = entry?.component;
  const cardId = item.card.id;

  useLayoutEffect(() => {
    const mounted = host.mount(cardRef.current);
    mountedRef.current = mounted;
    setCapabilities(mounted.card);
    return () => {
      mounted.unmount();
      if (mountedRef.current === mounted) {
        mountedRef.current = null;
        setCapabilities(null);
      }
    };
  }, [host, cardId]);

  useLayoutEffect(() => {
    mountedRef.current?.host.setVisible(visible);
    mountedRef.current?.host.setFocused(focused);
  }, [visible, focused]);

  if (Component === undefined || capabilities === null) {
    return (
      <div className="term">
        {/* An unknown card is exactly the one a reader most needs to be able to
            get rid of: no entry claims it, so nothing else on this board can
            act on it. The × is drawn here rather than left to the (absent)
            component. */}
        <CardHead
          className="card-drag-handle"
          title={item.title}
          onClose={onRemove}
          closeAriaLabel={`Delete card ${item.title}`}
        />
        <div className="term-body">
          <p className="term-line">{Component === undefined ? 'Unknown card' : item.title}</p>
        </div>
      </div>
    );
  }
  return <Component card={cardRef.current} host={capabilities} onRemove={onRemove} />;
}
