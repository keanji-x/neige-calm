// The conversation drawer. It overlays the panel column rather than squeezing the main column, and is deliberately not modal: no focus trap, no inert background. Escape closes it.

import { useEffect, useRef, type ReactNode } from 'react';

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

/** Focus `element` and read the result back — CSS-based prediction disagreed with the engine. `aria-hidden`/`inert` are checked by attribute first because `focus()` succeeds into them. */
function focusTook(element: HTMLElement): boolean {
  if (element.closest('[aria-hidden="true"], [inert]') !== null) return false;
  element.focus();
  return document.activeElement === element;
}

export function Drawer({ open, title, mobileBackLabel, closeLabel = 'Close conversation', onClose, children, footer }: {
  open: boolean;
  /** The drawer's accessible name; compact/mobile also paints it in the shared Header, desktop keeps it unpainted. */
  title: string;
  /** Accessible destination announced by the compact header's back control. */
  mobileBackLabel?: string;
  /** The desktop close control's accessible name; a drawer holding something other than the conversation says what it closes. */
  closeLabel?: string;
  onClose: () => void;
  children: ReactNode;
  /** Pinned below the scrolling body; a slot rather than the last child because the body scrolls and this must not. */
  footer?: ReactNode;
}) {
  const compact = useCompactViewport();
  const panelRef = useRef<HTMLDivElement | null>(null);
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
      aria-label={frame.title}
      tabIndex={-1}
      onAnimationEnd={() => { if (closing) setClosing(false); }}
    >
      {/* The close is before the scroller in the DOM so the first Tab out of the container lands on it. */}
      {compact ? (
        <div className={styles.mobileHeader}>
          <MobileHeader
            title={frame.title}
            backLabel={frame.mobileBackLabel}
            onBack={onClose}
          />
        </div>
      ) : (
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
      )}
      <div className={styles.scroll} data-nc-drawer-scroll="">
        <div className={styles.bodyInner}>
          {frame.children}
        </div>
      </div>
      {frame.footer}
    </div>
    {/* The seam is after the card in source order deliberately and is not marked `data-nc-drawer`: one drawer must present one marker for `app/shell`'s `:has()` rule. */}
    <div
      className={`${styles.seam} ${closing ? styles.seamClosing : ''}`}
      data-nc-drawer-seam=""
    />
    </>
  );
}
