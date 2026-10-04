// The conversation drawer. It overlays the panel column rather than squeezing the main column, and is deliberately not modal: no focus trap, no inert background. Escape closes it.

import { useEffect, useId, useLayoutEffect, useRef, type ReactNode } from 'react';

import { Icon } from '../icon/public.tsx';
import { MobileHeader } from '../mobile-header/public.tsx';
import { useState } from '../state/public.ts';
import { useCompactViewport } from '../viewport/public.ts';
import styles from './drawer.module.css';

/** The seam of the drawer `inside` is rendered in, or `null`; scoped through the card's parent so a second drawer's seam cannot be picked up. */
export function drawerSeamAround(inside: Element | null): HTMLElement | null {
  const card = inside?.closest('[data-nc-drawer]');
  return card?.parentElement?.querySelector<HTMLElement>('[data-nc-drawer-seam]') ?? null;
}

/** What the reader is on: a character of text, or the block at the probe line. */
type ReadingMark = Range | Element;
/** Where the reader is in the pane: at its end, or on `mark`, `top` px below the pane's top. */
type ReadingPlace = Readonly<{ atEnd: boolean; mark: ReadingMark | null; top: number }>;

/** How far below the pane's top the probe line sits: just under the header, past the body's own top padding. */
const READING_PROBE_PX = 12;

/** One character of text at the point, so a rewrapped paragraph is followed to the line being read. `?.` because jsdom has neither API. */
function characterAt(pane: HTMLElement, x: number, y: number): Range | null {
  const position = document.caretPositionFromPoint?.(x, y) ?? null;
  const caret = position === null ? document.caretRangeFromPoint?.(x, y) ?? null : null;
  const node = position?.offsetNode ?? caret?.startContainer ?? null;
  const offset = position?.offset ?? caret?.startOffset ?? 0;
  const length = node?.nodeType === Node.TEXT_NODE ? node.textContent?.length ?? 0 : 0;
  if (node === null || length === 0 || !pane.contains(node)) return null;
  const character = document.createRange();
  character.setStart(node, Math.min(offset, length - 1));
  character.setEnd(node, Math.min(offset, length - 1) + 1);
  return character;
}

/** With no text on the probe line (the gap between paragraphs, a margin), the deepest block that spans it, or the first block below it. */
function blockAt(pane: HTMLElement, y: number): Element | null {
  let box: Element = pane;
  for (;;) {
    const children = [...box.children];
    const spanning = children.find((child) => {
      const rect = child.getBoundingClientRect();
      return rect.height > 0 && rect.top <= y && rect.bottom > y;
    });
    if (spanning !== undefined) { box = spanning; continue; }
    return children.find((child) => child.getBoundingClientRect().top > y) ?? (box === pane ? null : box);
  }
}

function readingPlaceIn(pane: HTMLElement): ReadingPlace {
  const box = pane.getBoundingClientRect();
  const y = box.top + Math.min(READING_PROBE_PX, box.height / 2);
  const mark = characterAt(pane, box.left + box.width / 2, y) ?? blockAt(pane, y);
  return {
    atEnd: pane.scrollHeight - pane.scrollTop - pane.clientHeight <= 1,
    mark,
    top: mark === null ? 0 : mark.getBoundingClientRect().top - box.top,
  };
}

const markConnected = (mark: ReadingMark) => (mark instanceof Range ? mark.startContainer.isConnected : mark.isConnected);

/** Focus `element` and read the result back — CSS-based prediction disagreed with the engine. `aria-hidden`/`inert` are checked by attribute first because `focus()` succeeds into them. */
function focusTook(element: HTMLElement): boolean {
  if (element.closest('[aria-hidden="true"], [inert]') !== null) return false;
  element.focus();
  return document.activeElement === element;
}

/** Where the dragged edge went, in rem; `null` is the default width. The caller owns, applies and remembers it: `onPreview` once a frame while the edge moves, `onCommit` when it settles. The drawer stamps `data-nc-drawer-resizable` so the caller's stylesheet can apply the width only while this drawer is open. */
export type DrawerResize = Readonly<{ onPreview: (rem: number | null) => void; onCommit: (rem: number | null) => void }>;

/** One arrow press moves the edge this far. */
const KEY_STEP_REM = 1;

const remPx = () => parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;

export function Drawer({ open, title, mobileBackLabel, closeLabel = 'Close conversation', onClose, children, footer, resize }: {
  open: boolean;
  /** The drawer's accessible name, painted once: in the desktop header row, or in the shared compact Header. */
  title: string;
  /** Accessible destination announced by the compact header's back control. */
  mobileBackLabel?: string;
  /** The desktop close control's accessible name; a drawer holding something other than the conversation says what it closes. */
  closeLabel?: string;
  onClose: () => void;
  children: ReactNode;
  /** Pinned below the scrolling body; a slot rather than the last child because the body scrolls and this must not. */
  footer?: ReactNode;
  /** Absent: no resize handle and the default width. Compact viewports are already full width, so they never show it. */
  resize?: DrawerResize;
}) {
  const compact = useCompactViewport();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const titleId = useId();
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const handleRef = useRef<HTMLDivElement | null>(null);
  /* `pending` is the width the next frame applies; `moved` keeps a press without a drag from pinning the default to a number. */
  const drag = useRef<{
    pointerId: number; startX: number; startPx: number; place: ReadingPlace | null; frame: number; pending: number | null; moved: boolean;
  } | null>(null);
  const [dragging, setDragging] = useState(false);
  const width = compact ? undefined : resize;
  /* The card's share of its containing block, for the separator's value. Measured, not derived: the span's clamp belongs to the caller's stylesheet. */
  const [widthPercent, setWidthPercent] = useState<number | null>(null);
  useLayoutEffect(() => {
    const panel = panelRef.current;
    if (width === undefined || panel === null) return;
    const measure = () => {
      const region = panel.offsetParent?.clientWidth ?? 0;
      setWidthPercent(region > 0 ? Math.round(panel.getBoundingClientRect().width / region * 100) : null);
    };
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(panel);
    return () => { observer.disconnect(); };
  }, [width, open]);

  /* The browser's scroll anchoring stands down when an ancestor's width changes, so a reflowed transcript would otherwise move under the reader: the place is taken before the width moves and put back after, synchronously, since reading a rect lays the new width out. */
  const keepingReadingPlace = (place: ReadingPlace | null, change: () => void) => {
    change();
    const pane = scrollRef.current;
    if (place === null || pane === null) return;
    if (place.atEnd) pane.scrollTop = pane.scrollHeight;
    else if (place.mark !== null && markConnected(place.mark)) {
      pane.scrollTop += place.mark.getBoundingClientRect().top - pane.getBoundingClientRect().top - place.top;
    }
  };
  const placeNow = () => (scrollRef.current === null ? null : readingPlaceIn(scrollRef.current));
  /* The rendered width after the caller's clamp, so what is remembered is what is on screen. */
  const renderedRem = () => (panelRef.current?.getBoundingClientRect().width ?? 0) / remPx();
  const settle = (rem: number | null) => {
    if (width === undefined) return;
    keepingReadingPlace(placeNow(), () => { width.onPreview(rem); });
    width.onCommit(rem === null ? null : renderedRem());
  };
  const applyPending = () => {
    const current = drag.current;
    if (current === null || current.pending === null || width === undefined) return;
    const rem = current.pending;
    current.pending = null;
    current.moved = true;
    keepingReadingPlace(current.place, () => { width.onPreview(rem); });
  };
  const endDrag = () => {
    const current = drag.current;
    if (current === null) return;
    cancelAnimationFrame(current.frame);
    applyPending();
    drag.current = null;
    setDragging(false);
    if (handleRef.current?.hasPointerCapture(current.pointerId)) handleRef.current.releasePointerCapture(current.pointerId);
    if (current.moved) width?.onCommit(renderedRem());
  };
  /* A drawer that unmounts mid-drag owes no more frames. */
  useEffect(() => () => { if (drag.current !== null) cancelAnimationFrame(drag.current.frame); }, []);
  const [closing, setClosing] = useState(false);
  const wasOpen = useRef(open);
  const shouldRestoreFocus = useRef(false);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  /* The caller drops its selection the instant it asks for a close, so the retracting panel shows its last frame. A ref, not state: it must never cause a render. */
  const lastFrame = useRef<{
    title: string; mobileBackLabel?: string; children: ReactNode; footer?: ReactNode;
  }>(
    { title, mobileBackLabel, children, footer },
  );
  if (open) lastFrame.current = { title, mobileBackLabel, children, footer };
  const frame = open ? { title, mobileBackLabel, children, footer } : lastFrame.current;

  /* The retraction starts during render, not in an effect: an effect cost a frame in which the early return unmounted the drawer and then remounted it, replaying the enter animation. */
  if (open !== wasOpen.current) {
    // Only a true → false edge retracts; mounting closed does not.
    const retracts = wasOpen.current && !open && !compact
      && !globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    shouldRestoreFocus.current = wasOpen.current && !open;
    wasOpen.current = open;
    setClosing(retracts);
  }
  // A desktop exit may still be running when the viewport becomes compact.
  // Compact pages disappear in this commit; no mobile animationend is owed.
  if (compact && closing) setClosing(false);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      /* Escape during IME composition is the IME's Escape; closing on it would take the draft. `keyCode === 229` is kept only to match the router's copy of this guard — `isComposing` is the working fence. WebKit may dispatch `compositionend` before that Escape; unverified. */
      if (event.isComposing || event.keyCode === 229) return;
      const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) === panelRef.current) onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [open, onClose]);

  // `preventScroll` is load-bearing on open: the card enters translated, so a default `focus()` pans the page for a frame. Close restores without it so a scrolled-away opener is brought back into view.
  useEffect(() => {
    if (open) {
      const active = document.activeElement as HTMLElement | null;
      const panel = panelRef.current;
      /* Content that claimed the caret in the same commit (`focusOnMount`) made a more specific request; children's effects run first. Recording it as the opener would aim the restore at an element leaving the DOM. */
      if (panel !== null && active !== null && panel.contains(active)) return;
      previouslyFocusedRef.current = active;
      panel?.focus({ preventScroll: true });
      return;
    }
    /* `app/shell` hides the panel column off `[data-nc-drawer]` for the length of the exit animation, and the opener is almost always in that column, so restoring while `closing` is a silent no-op onto `<body>`. */
    if (!shouldRestoreFocus.current) return;
    const target = previouslyFocusedRef.current;
    /* `document.body` is excluded by hand: it answers `focus()` by keeping focus exactly where the failure mode puts it. */
    const openerTook = target !== null && target.isConnected
      && target !== document.body && focusTook(target);
    if (openerTook) {
      shouldRestoreFocus.current = false;
      return;
    }
    /* An opener that refused focus while `closing` may just be waiting for the animation; leave the restore armed for the rerun. A disconnected opener takes the fallback with no wait. */
    if (closing && target !== null && target.isConnected) return;
    shouldRestoreFocus.current = false;
    const fallback = document.querySelector<HTMLElement>('[data-nc-page-title]');
    if (fallback !== null && document.contains(fallback)) fallback.focus();
  }, [open, closing]);

  /* Compact pages and reduced motion skip the exit animation, so closing never waits for one the stylesheet does not play. */
  if (!open && !closing) return null;
  /* `data-nc-drawer` is the marker `app/shell` hides the trailing PanelCard by; a CSS Module class cannot be named across modules. It stays on during the closing animation. */
  return (
    <>
    <div
      ref={panelRef}
      className={`${styles.drawer} ${closing ? styles.drawerClosing : ''}`}
      role="complementary"
      data-nc-drawer=""
      data-nc-escape-layer={open ? '' : undefined}
      data-nc-drawer-resizable={width !== undefined ? '' : undefined}
      data-nc-drawer-resizing={dragging ? '' : undefined}
      /* Labelled once: by the painted desktop title, or, compact, by the name the shared Header also paints. */
      {...(compact ? { 'aria-label': frame.title } : { 'aria-labelledby': titleId })}
      tabIndex={-1}
      onAnimationEnd={() => { if (closing) setClosing(false); }}
    >
      {/* The header is before the scroller in the DOM, so the first Tab out of the container lands on its controls. */}
      {compact ? (
        <div className={styles.mobileHeader}>
          <MobileHeader
            title={frame.title}
            backLabel={frame.mobileBackLabel}
            onBack={onClose}
          />
        </div>
      ) : (
        <header className={styles.header}>
          <h2 id={titleId} className={styles.title} title={frame.title}>{frame.title}</h2>
          <button
            type="button"
            data-nc-role="icon"
            className={styles.close}
            aria-label={closeLabel}
            title="Close"
            onClick={onClose}
          >
            {/* A right chevron, not an X — the shape may not be shared with the page header's delete. */}
            <Icon name="chevron-right" />
          </button>
        </header>
      )}
      <div ref={scrollRef} className={styles.scroll} data-nc-drawer-scroll="">
        <div className={styles.bodyInner}>
          {frame.children}
        </div>
      </div>
      {frame.footer}
      {/* Last in the card so the first Tab off the container still lands on the header. The edge is the card's leading side; it widens toward the page. */}
      {width !== undefined && (
        /* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- a focusable separator is ARIA's window splitter, an operable widget; the plugin classes every separator as static. */
        <div
          ref={handleRef}
          className={styles.handle}
          role="separator"
          aria-orientation="vertical"
          aria-label="Resize conversation"
          aria-valuenow={widthPercent ?? undefined}
          /* eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- as above: the splitter takes focus so arrows and Home can move it. */
          tabIndex={0}
          title="Drag to resize · double-click to reset"
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
              place: placeNow(),
              frame: 0,
              pending: null,
              moved: false,
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
            settle(step === null ? null : renderedRem() + step);
          }}
        />
      )}
    </div>
    {/* The seam is after the card in source order deliberately and is not marked `data-nc-drawer`: one drawer must present one marker for `app/shell`'s `:has()` rule. */}
    <div
      className={`${styles.seam} ${closing ? styles.seamClosing : ''}`}
      data-nc-drawer-seam=""
    />
    </>
  );
}
