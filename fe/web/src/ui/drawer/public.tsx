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

export function Drawer({ open, label, title, onClose, children, footer }: {
  open: boolean;
  /**
   * What the surface is — "Conversation". The app's page headers carry a crumb
   * row above the title for the same reason: a title alone says what this is
   * *about* and leaves what it *is* to be inferred from context the drawer,
   * overlaying unrelated content, does not have.
   */
  label: string;
  /** What it is about — the wave. */
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

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose(); };
    document.addEventListener('keydown', onKeyDown);
    return () => { document.removeEventListener('keydown', onKeyDown); };
  }, [open, onClose]);

  // Focus moves in, because the drawer is what the click asked for; it is not
  // held there, because the drawer is not modal.
  useEffect(() => { if (open) panelRef.current?.focus(); }, [open]);

  if (!open) return null;
  return (
    <div
      ref={panelRef}
      className={styles.drawer}
      role="complementary"
      aria-label={title}
      tabIndex={-1}
      {...(scrolled ? { 'data-nc-scrolled': '' } : {})}
    >
      <div className={styles.head}>
        <div>
          <p className={styles.label}>{label}</p>
          <h2 className={styles.title}>{title}</h2>
        </div>
        <button
          type="button"
          data-nc-role="icon"
          className={styles.close}
          aria-label="Close conversation"
          title="Close"
          onClick={onClose}
        >
          ×
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
        {children}
      </div>
      {footer}
    </div>
  );
}
