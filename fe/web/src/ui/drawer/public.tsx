// §7.6's drawer, built to the spec that was written before it existed.
//
// It **overlays** the panel column, it does not squeeze the main column:
// squeezing would reflow the document every time it opens or closes, which is
// principle 3 ("在持续变化中保持安静") broken by the app's own chrome.
//
// It is deliberately **not modal** — no focus trap, no inert background, no
// overlay. A conversation is something you read *alongside* the page, and a
// trap would mean you cannot click the next wave without closing it first.
// Escape closes it, which is the one thing a non-modal overlay still owes you.

import { useEffect, useRef, type ReactNode } from 'react';

import { Icon } from '../icon/public.tsx';
import { useState } from '../state/public.ts';
import styles from './drawer.module.css';

/**
 * Ask `element` to take focus, and report whether it actually did.
 *
 * This used to be `canTakeFocus`, a *prediction*: connected, not `disabled`,
 * not under `aria-hidden`, and computed `visibility`/`display` both permissive.
 * Every clause of that was measured against Chromium and two of them were
 * backwards. `display` does not inherit, so reading it off the element says
 * nothing about a `display: none` **ancestor** — the exact case the docstring
 * claimed to cover — and `content-visibility: hidden` is invisible to it for
 * the same reason; both returned "yes, focusable" for an element `focus()`
 * cannot reach. In the other direction `aria-hidden` does not stop `focus()`
 * at all, so that clause vetoed targets that would have worked.
 *
 * Predicting focusability from CSS means re-deriving the engine's own
 * focusability rules by hand, and a wrong prediction here is not inert: the
 * caller uses it to decide whether to *wait*, so a false "yes" spends the one
 * armed restore on a `focus()` that silently no-ops and drops the opener.
 *
 * So there is no prediction. `focus()` is called and `document.activeElement`
 * is read back, which is the outcome itself and cannot disagree with the
 * engine. A `focus()` that does not take is a no-op — it does not move focus
 * anywhere else — so calling it speculatively costs nothing and leaves the
 * caller free to try again on the next render.
 *
 * jsdom implements `focus()` for real on genuinely focusable elements (and
 * computes no CSS, so nothing there is ever hidden); the CSS-driven failures
 * this exists for are therefore only observable in
 * `app/shell/drawer-seam.browser.test.tsx`, where the stylesheets are real.
 */
function focusTook(element: HTMLElement): boolean {
  element.focus();
  return document.activeElement === element;
}

export function Drawer({ open, title, onClose, children, footer }: {
  open: boolean;
  /**
   * The drawer's **accessible name**, and nothing that is painted.
   *
   * It used to be printed as an `<h2>` in a head band. The band is gone (see
   * the `.controls` note in the stylesheet), so this string now reaches the
   * reader only through `aria-label` on the container — which is where the
   * whole of its remaining value was anyway: a sighted reader clicked a named
   * conversation row to get here, a screen-reader user did not necessarily
   * land here from that row and still needs the region named.
   */
  title: string;
  onClose: () => void;
  children: ReactNode;
  /**
   * Pinned below the scrolling body — a composer, a confirm bar, whatever the
   * drawer is for. It is a slot rather than the last child of `children`
   * because the body scrolls and this must not: a message box that drifts off
   * the bottom of a long transcript is a message box you cannot reach.
   */
  footer?: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const [closing, setClosing] = useState(false);
  const wasOpen = useRef(open);
  const shouldRestoreFocus = useRef(false);
  const previouslyFocusedRef = useRef<HTMLElement | null>(null);

  /*
   * What a retracting drawer shows.
   *
   * The caller drops its selection the instant it asks for a close — the
   * conversation is gone from its state before this component renders again —
   * so without this the panel would slide out blank, which looks like a bug
   * rather than like a panel going away. Held in a ref, not state: it is a
   * snapshot of the last frame that had content, and it must never cause a
   * render of its own.
   */
  const lastFrame = useRef<{ title: string; children: ReactNode; footer?: ReactNode }>(
    { title, children, footer },
  );
  if (open) lastFrame.current = { title, children, footer };
  const frame = open ? { title, children, footer } : lastFrame.current;

  /*
   * The retraction starts **during render**, not in an effect, and that is the
   * whole of the fix for the flash on `›`.
   *
   * In an effect it cost a frame: `open` went false, this rendered with
   * `closing` still false, so the early return below unmounted the drawer — and
   * only then did the effect set `closing` and mount it again, replaying the
   * enter animation on the way out. Gone, back, slide out. One frame each way,
   * which is exactly what a flash is.
   *
   * Adjusting state while rendering is React's own answer for state that
   * derives from a prop change: the re-render happens before anything is
   * committed to the DOM, so there is no intermediate frame to see. The
   * `wasOpen` guard makes it run once per edge rather than every render.
   */
  if (open !== wasOpen.current) {
    // Only a true → false edge retracts; mounting closed does not.
    const retracts = wasOpen.current && !open
      && !globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    shouldRestoreFocus.current = wasOpen.current && !open;
    wasOpen.current = open;
    setClosing(retracts);
  }

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      /*
       * Escape during IME composition is the *IME's* Escape — it dismisses the
       * candidate list, and the browser delivers it here anyway. Closing on it
       * unmounts the composer and takes the draft with it, which for a bilingual
       * reader is a lost message every time a candidate is waved off.
       *
       * **`isComposing` is the whole of the working fence.** `keyCode === 229`
       * is kept only so this reads identically to the router's copy of the same
       * guard (`app/router/public.tsx`), and a reader should not believe it
       * catches anything here: the `event.key !== 'Escape'` line above runs
       * first, and every engine path that still reports `keyCode === 229`
       * reports `key` as `'Process'` or `'Unidentified'`, so it has already
       * returned. Verified by mutation — deleting the `keyCode` clause leaves
       * browser 26/26 and web-dom 750/750 green, so nothing anywhere is
       * standing on it.
       *
       * **Known gap, not solved here:** the test bed is Chromium only. WebKit
       * has historically dispatched `compositionend` *before* the Escape that
       * dismissed the candidate list, which would deliver that Escape with
       * `isComposing === false` and close the drawer under a Safari reader
       * mid-composition. Unverified on current WebKit; recorded so the next
       * person measures it rather than assuming this fence is complete.
       */
      if (event.isComposing || event.keyCode === 229) return;
      const layers = document.querySelectorAll<HTMLElement>('[data-nc-escape-layer]');
      if (layers.item(layers.length - 1) === panelRef.current) onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [open, onClose]);

  // Focus moves in, because the drawer is what the click asked for; it is not
  // held there, because the drawer is not modal.
  //
  // `preventScroll` is load-bearing on open. The card is `position: absolute`
  // inside `.main` and enters translated, so the first painted box is 12px off
  // where it lands. A default `focus()` asks the browser to scroll that box
  // into view, which pans the page for a frame — the jump clicking a
  // conversation card used to make.
  // Close restores without it: Today does not pin the conversation card, and
  // a keyboard user who scrolled the page behind the drawer still needs the
  // opener brought back into view.
  useEffect(() => {
    if (open) {
      previouslyFocusedRef.current = document.activeElement as HTMLElement | null;
      panelRef.current?.focus({ preventScroll: true });
      return;
    }
    /*
     * Restoring waits for `closing` to clear, and that ordering is the fix, not
     * a nicety.
     *
     * `app/shell` hides the whole panel column off this drawer's own marker —
     * `.main:has([data-nc-drawer]) [data-nc-panel] { visibility: hidden }` — and
     * the marker stays on for the exit animation. The opener is almost always a
     * row *in that column*, so restoring while `closing` is true aims `focus()`
     * at a `visibility: hidden` element: the call is silently a no-op, the
     * document keeps focus on `<body>`, and the next Tab restarts from the top
     * of the page. Waiting one animation means the drawer is out of the DOM,
     * the `:has()` no longer matches, and the opener is a real target again.
     */
    if (!shouldRestoreFocus.current) return;
    const target = previouslyFocusedRef.current;
    /*
     * The opener gets the first ask, and the ask *is* the test — see
     * `focusTook`. `document.body` is excluded by hand because it answers
     * `focus()` by keeping focus exactly where the failure mode puts it, so a
     * drawer opened from nothing in particular would "succeed" onto `<body>`,
     * which is the one outcome this whole effect exists to prevent.
     */
    const openerTook = target !== null && target.isConnected
      && target !== document.body && focusTook(target);
    if (openerTook) {
      shouldRestoreFocus.current = false;
      return;
    }
    /*
     * While `closing` is true the marker is still on and so is the hiding rule
     * (`app/shell` hides the panel column off `[data-nc-drawer]`), so an opener
     * that just refused focus may simply be waiting for the animation to end.
     * Leave `shouldRestoreFocus` armed and let the rerun this effect gets when
     * `closing` clears do the work — falling through to the page title here
     * would throw the opener away for a state that lasts 200ms. An opener that
     * has left the DOM is a different answer and not a slow one, so
     * `isConnected` keeps it on the fallback path with no wait.
     */
    if (closing && target !== null && target.isConnected) return;
    shouldRestoreFocus.current = false;
    const fallback = document.querySelector<HTMLElement>('[data-nc-page-title]');
    if (fallback !== null && document.contains(fallback)) fallback.focus();
  }, [open, closing]);

  /*
   * The drawer **leaves**; it does not vanish.
   *
   * §7.6 said enter animates and exit is instant, on the reasoning that an exit
   * transition keeps the screen busy after the decision is made. That is right
   * for a dialog, which is a thing that was in the way and is now gone, and it
   * is wrong here: closing a conversation does not end it, and an instant
   * disappearance is the vocabulary for something being destroyed. It goes out
   * the way it came in — 12px and a fade, reversed — which is the mildest thing
   * that still reads as "put away" rather than "gone".
   *
   * So closing holds the element mounted for one animation. Reduced motion
   * skips the phase entirely rather than waiting on an `animationend` that a
   * suppressed animation will never fire.
   */
  if (!open && !closing) return null;
  /*
   * `data-nc-drawer` is the marker `app/shell` hides the trailing PanelCard by.
   * The drawer is now a card on the panel's own track, so an unhidden panel
   * shows as a sliver of card peeking out from under it; a CSS Module class
   * cannot be named from another module's stylesheet, so the two ends of that
   * rule meet on a data attribute instead. It stays on during the closing
   * animation — the panel reappears when this unmounts, one frame after the
   * card has finished going away.
   */
  return (
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
      {/*
        * The close floats over the card's top-inline-end corner, and it is
        * **before** the scroller in the DOM so the first Tab out of the
        * container lands on it, which is the order the `.drawer:focus-visible`
        * note in the stylesheet assumes.
        *
        * It used to sit in a `.controls` flex group beside the reset. The reset
        * left the corner, and has since left the product altogether (#1139),
        * so the group had one member, and a wrapper whose only job was to
        * space two things is not kept for one. The floating geometry moved
        * onto `.close` itself; nothing about where the chevron lands changed.
        */}
      <button
        type="button"
        data-nc-role="icon"
        className={styles.close}
        aria-label="Close conversation"
        title="Close"
        onClick={onClose}
      >
        {/* A right chevron, not an X — see the `.close` note in the stylesheet
            for why the shape may not be shared with the page header's
            delete. */}
        <Icon name="chevron-right" />
      </button>
      <div className={styles.scroll} data-nc-drawer-scroll="">
        <div className={styles.bodyInner}>
          {frame.children}
        </div>
      </div>
      {frame.footer}
    </div>
  );
}
