// §7.6's drawer, built to the spec that was written before it existed.
//
// It **overlays** the panel column, it does not squeeze the main column:
// squeezing would reflow the document every time it opens or closes, which is
// principle 3 ("在持续变化中保持安静") broken by the app's own chrome.
//
// It is deliberately **not modal** — no focus trap, no inert background, no
// overlay. A conversation is something you read *alongside* the page, and a
// trap would mean you cannot click the next track without closing it first.
// Escape closes it, which is the one thing a non-modal overlay still owes you.

import { useEffect, useRef, type ReactNode } from 'react';

import { Icon } from '../icon/public.tsx';
import { MobileHeader } from '../mobile-header/public.tsx';
import { useState } from '../state/public.ts';
import { useCompactViewport } from '../viewport/public.ts';
import styles from './drawer.module.css';

/**
 * The marker on the drawer's seam — the strip of page between the card's
 * trailing edge and the window — for a caller that has something to put beside
 * the card rather than inside it.
 *
 * **Why a data attribute and not a context or a prop.** The thing that needs
 * the seam is `features/chat`'s exchange rail, several layers below the
 * `<Drawer>` element: the router composes `<Drawer><ChatThread/></Drawer>` and
 * the rail is `ChatThread`'s own child. A prop would have to be declared and
 * forwarded by every component in between, none of which has any use for it. A
 * context would work, but `architecture/no-create-context-outside-allowlist`
 * governs those by an allowlist with a written reason per entry, and expanding
 * a governance list is a heavier change than this needs.
 *
 * What it needs is the mechanism this seam already uses everywhere else.
 * `features/chat` finds the drawer's scrolling pane with
 * `closest('[data-nc-drawer-scroll]')`, and `app/shell` hides the panel column
 * off `[data-nc-drawer]` for the reason recorded there: a CSS Module class
 * cannot be named from another module, so the two ends of a cross-module seam
 * meet on a data attribute. This is the same seam and the same idiom — one
 * more attribute, no new mechanism, and it stays greppable from both sides.
 *
 * The attribute is written out as a literal at both ends rather than shared
 * through a constant, which is what `data-nc-drawer` and
 * `data-nc-drawer-scroll` already do — and here it is not merely convention:
 * `architecture/no-class-dom-query` requires a runtime selector to be a static
 * string, and a template built from a constant fails it closed.
 */

/**
 * The seam belonging to the drawer `inside` is rendered in, or `null`.
 *
 * Scoped through the drawer rather than taken off the document, so it cannot
 * pick up a second drawer's seam: the card and the seam are siblings, so the
 * card's parent is the box that holds exactly this pair. `null` means there is
 * no drawer above `inside` — a transcript rendered in place has no seam and
 * therefore no rail, which is the honest answer rather than a fallback, since
 * the rail's whole geometry *is* the seam.
 */
export function drawerSeamAround(inside: Element | null): HTMLElement | null {
  const card = inside?.closest('[data-nc-drawer]');
  return card?.parentElement?.querySelector<HTMLElement>('[data-nc-drawer-seam]') ?? null;
}

/**
 * Ask `element` to take focus, and report whether the focus landed somewhere a
 * reader can actually be.
 *
 * **Two different questions, answered two different ways, and the split is the
 * point.**
 *
 * *Can the engine put focus here?* is decided by **calling `focus()` and
 * reading `document.activeElement` back**. This used to be `canTakeFocus`, a
 * prediction from computed CSS — connected, not `disabled`, `visibility` and
 * `display` both permissive — and two of its clauses were backwards against
 * Chromium. `display` does not inherit, so reading it off the element says
 * nothing about a `display: none` **ancestor**, the exact case the docstring
 * claimed to cover; `content-visibility: hidden` is invisible to it for the
 * same reason. Both answered "yes, focusable" for an element `focus()` cannot
 * reach, and a false yes is not inert here — the caller uses it to decide
 * whether to *wait*, so it spends the one armed restore on a silent no-op and
 * drops the opener. Predicting focusability from CSS is re-deriving the
 * engine's rules by hand; the outcome cannot disagree with the engine, so the
 * outcome is what is read. A `focus()` that does not take moves focus nowhere,
 * so asking speculatively costs nothing.
 *
 * *Should focus be here even though the engine allows it?* cannot be answered
 * that way, because `focus()` **succeeds** into an `aria-hidden` or `inert`
 * subtree — `aria-hidden` is an accessibility-tree statement with no effect on
 * focusability at all, and `inert`'s own removal is not observable through
 * `activeElement` on every path. A landing there reads back as a triumph while
 * the target does not exist in the tree a screen reader is walking: the reader
 * is told nothing, and the caller cancels the fallback that would have put them
 * somewhere real. So these two are checked **by attribute, before the ask** —
 * the old predicate had this clause and it was right; what was wrong with it
 * was the CSS half, which is gone.
 *
 * `closest()` and not a computed read, because both attributes inherit down the
 * subtree by definition rather than by cascade, and that is precisely what
 * `closest()` walks.
 *
 * jsdom implements `focus()` for real on genuinely focusable elements (and
 * computes no CSS, so nothing there is ever hidden); the CSS-driven failures
 * the `focus()` half exists for are therefore only observable in
 * `app/shell/drawer-seam.browser.test.tsx`, where the stylesheets are real. The
 * attribute half is engine-independent and is pinned at the unit tier.
 */
function focusTook(element: HTMLElement): boolean {
  if (element.closest('[aria-hidden="true"], [inert]') !== null) return false;
  element.focus();
  return document.activeElement === element;
}

export function Drawer({ open, title, mobileBackLabel, onClose, children, footer }: {
  open: boolean;
  /**
   * The drawer's accessible name. Compact/mobile also paints it in the shared
   * Header; desktop keeps the title unpainted to preserve the side-card shape.
   *
   * It used to be printed as an `<h2>` in a head band. The band is gone (see
   * the `.controls` note in the stylesheet); mobile's page Header is a separate
   * responsive presentation, not a restoration of that desktop band.
   */
  title: string;
  /** Accessible destination announced by the compact header's back control. */
  mobileBackLabel?: string;
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
  const compact = useCompactViewport();
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
  const lastFrame = useRef<{
    title: string; mobileBackLabel?: string; children: ReactNode; footer?: ReactNode;
  }>(
    { title, mobileBackLabel, children, footer },
  );
  if (open) lastFrame.current = { title, mobileBackLabel, children, footer };
  const frame = open ? { title, mobileBackLabel, children, footer } : lastFrame.current;

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
      const active = document.activeElement as HTMLElement | null;
      const panel = panelRef.current;
      /*
       * Unless something inside has already claimed it.
       *
       * A drawer whose content asked for the caret in the same commit that
       * opened it — `ChatComposer`'s `focusOnMount`, the landing a
       * just-created track gets (#1211 S2) — has made a *more specific* request
       * than "focus moves in", and children's effects run before this one. So
       * this would not be moving focus in; it would be pulling it back out of
       * the one control the reader was put in front of. It would also record
       * an opener that lives inside this drawer, which the close-restore below
       * would then aim at an element that is on its way out of the DOM.
       */
      if (panel !== null && active !== null && panel.contains(active)) return;
      previouslyFocusedRef.current = active;
      panel?.focus({ preventScroll: true });
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
          aria-label="Close conversation"
          title="Close"
          onClick={onClose}
        >
          {/* A right chevron, not an X — see the `.close` note in the stylesheet
              for why the shape may not be shared with the page header's
              delete. */}
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
    {/*
      * The seam, and it is **after** the card in source order deliberately —
      * see `.seam` in the stylesheet for what that buys and what it costs.
      * It carries the card's own closing class so the two move as one object,
      * and it is not marked `data-nc-drawer`: `app/shell` hides the panel
      * column off that marker with `:has()`, and one drawer must present one
      * marker or the rule's set-equality with "routes that render a drawer"
      * stops being checkable.
      */}
    <div
      className={`${styles.seam} ${closing ? styles.seamClosing : ''}`}
      data-nc-drawer-seam=""
    />
    </>
  );
}
