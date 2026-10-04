// The drawer's resize edge: a focusable window splitter down the card's leading side. Its mount is the drag's lifetime — the drawer renders it only while open on a desktop viewport with a caller's contract, so a close, a compact viewport or a lost contract unmounts it, and the unmount ends a drag still held, keeping the width last laid out.

import { useEffect, useLayoutEffect, useRef, type RefObject } from 'react';

import { useState } from '../state/public.ts';
import styles from './drawer.module.css';
import { readingPlaceIn, restoreReadingPlace, type ReadingPlace } from './reading-place.ts';

/** Where the dragged edge went, in rem; `null` is the default width. The caller owns, applies and remembers it: `onPreview` once a frame while the edge moves, `onCommit` when it settles. The drawer stamps `data-nc-drawer-resizable` so the caller's stylesheet can apply the width only while this drawer is open. */
export type DrawerResize = Readonly<{ onPreview: (rem: number | null) => void; onCommit: (rem: number | null) => void }>;

/** One arrow press moves the edge this far. */
const KEY_STEP_REM = 1;

const remPx = () => parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;

/* `pending` is the width the next frame applies. `settled` is the rendered width after the last applied frame — what is on screen once the caller's clamp has run — and stays `null` until the edge moves, so a press without a drag never pins the default to a number. */
type Drag = {
  pointerId: number; startX: number; startPx: number; place: ReadingPlace | null; frame: number; pending: number | null; settled: number | null;
};

export function ResizeEdge({ resize, panelRef, scrollRef }: {
  resize: DrawerResize;
  panelRef: RefObject<HTMLDivElement | null>;
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const drag = useRef<Drag | null>(null);
  const [dragging, setDragging] = useState(false);
  /* The card's share of its containing block, for the separator's value. Measured, not derived: the span's clamp belongs to the caller's stylesheet. A passive effect: the card's ref is the parent's, attached after this child's layout effects run. */
  const [percent, setPercent] = useState<number | null>(null);
  useEffect(() => {
    const panel = panelRef.current;
    if (panel === null) return;
    const measure = () => {
      const region = panel.offsetParent?.clientWidth ?? 0;
      setPercent(region > 0 ? Math.round(panel.getBoundingClientRect().width / region * 100) : null);
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(panel);
    return () => { observer.disconnect(); };
  }, [panelRef]);

  /* The unmount commits through the contract of the last render, not the first. A layout cleanup, so the drag ends in the commit that removes the edge: a passive one may run after the next frame, which would lay out a queued desktop width on a card that is now compact, or gone. */
  const latestResize = useRef(resize);
  useLayoutEffect(() => { latestResize.current = resize; });
  useLayoutEffect(() => () => {
    const current = drag.current;
    if (current === null) return;
    drag.current = null;
    cancelAnimationFrame(current.frame);
    if (current.settled !== null) latestResize.current.onCommit(current.settled);
  }, []);

  /** Move the edge to `rem` with the reader kept on `place`, and return the width laid out. */
  const resizeTo = (place: ReadingPlace | null, rem: number | null) => {
    resize.onPreview(rem);
    restoreReadingPlace(scrollRef.current, place);
    return (panelRef.current?.getBoundingClientRect().width ?? 0) / remPx();
  };
  const settle = (rem: number | null) => {
    const laid = resizeTo(scrollRef.current === null ? null : readingPlaceIn(scrollRef.current), rem);
    resize.onCommit(rem === null ? null : laid);
  };
  const applyPending = () => {
    const current = drag.current;
    if (current === null || current.pending === null) return;
    const rem = current.pending;
    current.pending = null;
    current.settled = resizeTo(current.place, rem);
  };
  const endDrag = () => {
    const current = drag.current;
    if (current === null) return;
    cancelAnimationFrame(current.frame);
    applyPending();
    drag.current = null;
    setDragging(false);
    if (current.settled !== null) resize.onCommit(current.settled);
  };

  return (
    /* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- a focusable separator is ARIA's window splitter, an operable widget; the plugin classes every separator as static. */
    <div
      className={styles.handle}
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize conversation"
      aria-valuenow={percent ?? undefined}
      /* eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- as above: the splitter takes focus so arrows and Home can move it. */
      tabIndex={0}
      title="Drag to resize · double-click to reset"
      data-nc-resizing={dragging ? '' : undefined}
      /* No focus and no text selection from a press: the drag must leave the caret where the reader left it. */
      onMouseDown={(event) => { event.preventDefault(); }}
      onPointerDown={(event) => {
        if (event.button !== 0 || drag.current !== null) return;
        event.preventDefault();
        event.currentTarget.setPointerCapture(event.pointerId);
        drag.current = {
          pointerId: event.pointerId,
          startX: event.clientX,
          startPx: panelRef.current?.getBoundingClientRect().width ?? 0,
          place: scrollRef.current === null ? null : readingPlaceIn(scrollRef.current),
          frame: 0,
          pending: null,
          settled: null,
        };
        setDragging(true);
      }}
      onPointerMove={(event) => {
        const current = drag.current;
        if (current === null || event.pointerId !== current.pointerId) return;
        const pending = current.pending;
        current.pending = (current.startPx + current.startX - event.clientX) / remPx();
        if (pending === null) current.frame = requestAnimationFrame(applyPending);
      }}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onLostPointerCapture={endDrag}
      onDoubleClick={() => { settle(null); }}
      onKeyDown={(event) => {
        const step = event.key === 'ArrowLeft' ? KEY_STEP_REM : event.key === 'ArrowRight' ? -KEY_STEP_REM : null;
        if (step === null && event.key !== 'Home') return;
        event.preventDefault();
        const panel = panelRef.current;
        settle(step === null || panel === null ? null : panel.getBoundingClientRect().width / remPx() + step);
      }}
    />
  );
}
