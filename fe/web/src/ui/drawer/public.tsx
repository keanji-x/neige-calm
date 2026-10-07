// The conversation drawer. It overlays the panel column rather than squeezing the main column, and is deliberately not modal: no focus trap, no inert background. Escape closes it.

import { useEffect, useId, useLayoutEffect, useMemo, useRef, type ReactNode } from 'react';
import { LayerDepthProvider, useLayerDismissal } from '@astryxdesign/core/Layer';

import { Icon } from '../icon/public.tsx';
import { MobileHeader } from '../mobile-header/public.tsx';
import { useState } from '../state/public.ts';
import { useCompactViewport } from '../viewport/public.ts';
import styles from './drawer.module.css';
import { useSpringPresence } from '../motion/presence.ts';
import type { PaneResizeGroup } from './resize-group.ts';
import { ResizeEdge, type DrawerResize } from './resize-edge.tsx';

export type { DrawerResize } from './resize-edge.tsx';

/** The seam of the drawer `inside` is rendered in, or `null`; scoped through the card's parent so a second drawer's seam cannot be picked up. */
export function drawerSeamAround(inside: Element | null): HTMLElement | null {
  const card = inside?.closest('[data-nc-drawer]');
  return card?.parentElement?.querySelector<HTMLElement>('[data-nc-drawer-seam]') ?? null;
}

/** Focus `element` and read the result back — CSS-based prediction disagreed with the engine. `aria-hidden`/`inert` are checked by attribute first because `focus()` succeeds into them. */
function focusTook(element: HTMLElement): boolean {
  if (element.closest('[aria-hidden="true"], [inert]') !== null) return false;
  element.focus();
  return document.activeElement === element;
}

export function Drawer({ open, title, mobileBackLabel, mobileHeader, closeLabel = 'Close conversation', onClose, children, footer, resize, companion, inline = false, id, stacked = false, resizeGroup: suppliedResizeGroup = null }: {
  open: boolean;
  id?: string;
  /** An independent second card displayed below this one, or switched on compact screens. */
  companion?: (group: PaneResizeGroup) => ReactNode;
  /** Explicitly shares the host allocation and scrolling panes with a companion. */
  resizeGroup?: PaneResizeGroup | null;
  /** Keep the primary card mounted as companions open and close. */
  stacked?: boolean;
  /** Render inside a companion slot rather than positioning another overlay. */
  inline?: boolean;
  /** The drawer's accessible name, painted once: in the desktop header row, or in the shared compact Header. */
  title: string;
  /** Accessible destination announced by the compact header's back control. */
  mobileBackLabel?: string;
  /** Optional compact header supplied by the host for a modal presentation. */
  mobileHeader?: ReactNode;
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
  const seamRef = useRef<HTMLDivElement | null>(null);
  const parentResizeGroup = suppliedResizeGroup;
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [panes] = useState(() => new Set<HTMLElement>());
  const ownResizeGroup = useMemo<PaneResizeGroup>(() => ({ host: hostRef, panes }), [panes]);
  const resizeGroup = parentResizeGroup ?? (stacked || companion !== undefined ? ownResizeGroup : null);

  const width = compact ? undefined : resize;
  const [closing, setClosing] = useState(false);
  useLayoutEffect(() => {
    const pane = scrollRef.current;
    if (pane === null || resizeGroup === null) return;
    resizeGroup.panes.add(pane);
    return () => { resizeGroup.panes.delete(pane); };
  }, [resizeGroup, open, closing]);
  const wasOpen = useRef(open);
  const shouldRestoreFocus = useRef(false);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  // A companion may be removed by its owner instead of receiving open=false.
  // Restore while its focused DOM is still attached, before React removes it.
  useLayoutEffect(() => () => {
    const pane = panelRef.current;
    if (pane === null || !pane.contains(document.activeElement)) return;
    const target = previouslyFocusedRef.current;
    if (target !== null && target.isConnected && target !== document.body && focusTook(target)) return;
    document.querySelector<HTMLElement>('[data-nc-page-title]')?.focus();
  }, []);


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
  // Compact pages disappear in this commit; no mobile transitionend is owed.
  if (compact && closing) setClosing(false);

  useSpringPresence(panelRef, seamRef, open, open || closing, !compact, () => { setClosing(false); }, value => ({
    opacity: Math.max(0, Math.min(1, value)),
    translate: `0 calc(var(--space-6) * ${1 - value})`,
  }));

  // Keep the local escape-layer contract for unmigrated Dialog/file-viewer hosts;
  // native Astryx children still dismiss first through the shared layer stack.
  useLayerDismissal({
    isActive: open,
    onDismiss: onClose,
    getContainer: () => panelRef.current,
    isPresent: () => {
      const layers = [...document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]')]
        .filter(layer => layer.closest('[hidden]') === null);
      return open && panelRef.current !== null && layers.at(-1) === panelRef.current;
    },
  });

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

  /* Compact pages and reduced motion skip the exit transition, so closing never waits for one the stylesheet does not play. */
  if (!open && !closing) return null;
  /* `data-nc-drawer` is the marker `app/shell` hides the trailing PanelCard by; a CSS Module class cannot be named across modules. It stays on during the closing animation. */
  const card = (
    <>
    <div
      id={id}
      ref={panelRef}
      className={`${styles.drawer} ${inline ? styles.embedded : ''} ${closing ? styles.drawerClosing : ''}`}
      role={inline ? "region" : "complementary"}
      data-nc-drawer=""
      data-nc-escape-layer={open ? '' : undefined}
      data-nc-drawer-resizable={width !== undefined ? '' : undefined}
      /* Labelled once: by the painted desktop title, or, compact, by the name the shared Header also paints. */
      {...(compact && !inline ? { 'aria-label': frame.title } : { 'aria-labelledby': titleId })}
      tabIndex={-1}

    >
      {/* The header is before the scroller in the DOM, so the first Tab out of the container lands on its controls. */}
      {compact && !inline ? (
        <div className={styles.mobileHeader}>
          {mobileHeader ?? <MobileHeader
            title={frame.title}
            backLabel={frame.mobileBackLabel}
            onBack={onClose}
          />}
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
      {/* Last in the card so the first Tab off the container still lands on the header. Mounted only while open: the edge's lifetime is the drag's, so a close mid-drag ends it. */}
      {open && width !== undefined && !stacked && companion === undefined && parentResizeGroup === null
        && <ResizeEdge resize={width} panelRef={panelRef} scrollRef={scrollRef} group={resizeGroup} />}
    </div>
    {/* The seam is after the card in source order deliberately and is not marked `data-nc-drawer`: one drawer must present one marker for `app/shell`'s `:has()` rule. */}
    <div
      ref={seamRef}
      className={`${styles.seam} ${closing ? styles.seamClosing : ''}`}
      data-nc-drawer-seam=""
    />
    </>
  );
  if (companion === undefined && !stacked) return <LayerDepthProvider>{card}</LayerDepthProvider>;
  return (
    <div ref={hostRef} className={styles.stack} data-nc-drawer-stack="">
      <div className={styles.cell}><LayerDepthProvider>{card}</LayerDepthProvider></div>
      <div className={styles.cell} hidden={companion === undefined}>{companion?.(ownResizeGroup)}</div>
      {open && width !== undefined && parentResizeGroup === null
        && <ResizeEdge resize={width} panelRef={hostRef} scrollRef={scrollRef} group={resizeGroup} />}
    </div>
  );
}
