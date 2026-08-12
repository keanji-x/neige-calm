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

import { useEffect, useRef, type ReactNode, type UIEvent } from 'react';

import { useState } from '../state/public.ts';
import styles from './drawer.module.css';

export function Drawer({ open, title, onClose, children, footer }: {
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
}) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  const [scrolled, setScrolled] = useState(false);
  const [closing, setClosing] = useState(false);
  const wasOpen = useRef(open);

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
  const lastFrame = useRef<{ title: string; children: ReactNode; footer?: ReactNode }>({ title, children, footer });
  if (open) lastFrame.current = { title, children, footer };
  const frame = open ? { title, children, footer } : lastFrame.current;

  useEffect(() => {
    if (open === wasOpen.current) return;
    // Only a true → false edge starts a retraction; mounting closed does not.
    const retracts = wasOpen.current && !open
      && !globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    wasOpen.current = open;
    setClosing(retracts);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [open, onClose]);

  // Focus moves in, because the drawer is what the click asked for; it is not
  // held there, because the drawer is not modal.
  useEffect(() => { if (open) panelRef.current?.focus(); }, [open]);

  /*
   * The drawer **retracts**; it does not vanish.
   *
   * §7.6 said enter animates and exit is instant, on the reasoning that an exit
   * transition keeps the screen busy after the decision is made. That is right
   * for a dialog, which is a thing that was in the way and is now gone. It is
   * wrong for a panel attached to an edge: the whole point of an edge panel is
   * that it is *still there*, pushed off-screen, and an instant disappearance
   * says it was destroyed. The control says the same thing — `›`, the direction
   * it goes, the same glyph the rail's own collapse uses.
   *
   * So closing holds the element mounted for one animation. Reduced motion
   * skips the phase entirely rather than waiting on an `animationend` that a
   * suppressed animation will never fire.
   */
  if (!open && !closing) return null;
  return (
    <div
      ref={panelRef}
      className={`${styles.drawer} ${closing ? styles.drawerClosing : ''}`}
      role="complementary"
      aria-label={frame.title}
      tabIndex={-1}
      onAnimationEnd={() => { if (closing) setClosing(false); }}
      {...(scrolled ? { 'data-nc-scrolled': '' } : {})}
    >
      <div className={styles.head}>
        <h2 className={styles.title}>{frame.title}</h2>
        <button
          type="button"
          data-nc-role="icon"
          className={styles.close}
          aria-label="Close conversation"
          title="Close"
          onClick={onClose}
        >
          ›
        </button>
      </div>
      <div
        className={styles.body}
        onScroll={(event: UIEvent<HTMLDivElement>) => {
          // The head's rule appears only once something is under it. Compared
          // against the current value so a scroll event per frame does not
          // become a render per frame.
          const past = event.currentTarget.scrollTop > 0;
          if (past !== scrolled) setScrolled(past);
        }}
      >
        {frame.children}
      </div>
      {frame.footer}
    </div>
  );
}
