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

import { useEffect, useRef, type ReactElement, type ReactNode } from 'react';

import { Icon } from '../icon/public.tsx';
import { useState } from '../state/public.ts';
import styles from './drawer.module.css';

/**
 * A control in the drawer's head, beside the close.
 *
 * It is a companion component rather than a free `ReactNode` for the reason
 * `PanelAction` is one: the geometry belongs to the head — a 28px hit area
 * carried on the title's first line — and a caller composing its own button
 * would have to restate it, which is the drift the role/tier split exists to
 * prevent (§4.1).
 *
 * `danger` is §4.3's tier and it is red **at rest**: a warning that appears
 * only under the pointer is missing at the moment of the decision, and missing
 * from the keyboard path entirely. Icon-only, so the label is the whole of what
 * a screen reader gets — it must name the object, not just the verb.
 */
export function DrawerAction({ label, onClick, danger = false, children }: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: ReactElement;
}) {
  return (
    <button
      type="button"
      data-nc-role="icon"
      className={`${styles.action} ${danger ? styles.actionDanger : ''}`}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

export function Drawer({ open, title, onClose, children, footer, headAction }: {
  open: boolean;
  /**
   * The whole head — one grey line on the close button's row.
   *
   * It briefly carried a "CONVERSATION" eyebrow above this, on the theory that
   * a title says what the surface is *about* and leaves what it *is* to be
   * inferred. In a drawer that only ever opens from a conversation control,
   * with a transcript and a message box under it, nothing was left to infer:
   * the word was a caption on an unambiguous thing, and it cost the head a
   * whole line and a second type rank to say it.
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
  /**
   * One control beside the close, for something that belongs to the surface
   * rather than to what is in it — today, the conversation's reset.
   *
   * It is a head slot and not a footer button because the footer is where you
   * *work*: a destructive action standing next to the message box is one
   * mis-click away from the most routine thing on the surface, and it inherits
   * the visual weight of a control you press every turn.
   */
  headAction?: ReactNode;
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
  const lastFrame = useRef<{ title: string; children: ReactNode; footer?: ReactNode; headAction?: ReactNode }>(
    { title, children, footer, headAction },
  );
  if (open) lastFrame.current = { title, children, footer, headAction };
  const frame = open ? { title, children, footer, headAction } : lastFrame.current;

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
    if (!shouldRestoreFocus.current) return;
    shouldRestoreFocus.current = false;
    const target = previouslyFocusedRef.current;
    const fallback = document.querySelector<HTMLElement>('[data-nc-page-title]');
    const destination = target && document.contains(target) ? target : fallback;
    if (destination && document.contains(destination)) destination.focus();
  }, [open]);

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
      <div className={styles.scroll} data-nc-drawer-scroll="">
        <div className={styles.head}>
          <h2 className={styles.title}>{frame.title}</h2>
          <div className={styles.headActions}>
            {frame.headAction}
            <button
              type="button"
              data-nc-role="icon"
              className={styles.close}
              aria-label="Close conversation"
              title="Close"
              onClick={onClose}
            >
              <Icon name="close" />
            </button>
          </div>
        </div>
        <div className={styles.bodyInner}>
          {frame.children}
        </div>
      </div>
      {frame.footer}
    </div>
  );
}
