// The panel column's one card, and the modules stacked inside it.
//
// Every route has the same skeleton now: an unbounded report document in the
// main column, and **one** rounded card top-right holding two modules — the
// route's own (Today's calendar, a cove's wave list, a wave's cards) and the
// conversation list, which is the same on all three.
//
// §6.5 says a panel-column panel draws no border of its own. That rule was
// written for a panel column holding one thing, where the 24px gutter already
// said where the panel began. It now holds two modules of different function,
// and the gutter cannot say they are one object — only the container can. So
// the container is drawn, and it is drawn with step ③ of §6.5's decision ladder
// alone (different function → change surface): `--surface-card` sits 3.4 L from
// `--bg` in light and 6.9 in dark, both above the 3.0 L line at which a surface
// step stands as a boundary by itself. Above that line the ladder *forbids*
// spending a hairline on the same boundary, so the card has no outline — see
// `panel-card.module.css`. The hairlines inside it separate module from module,
// which is a different boundary (step ②).
//
// Still no shadow: §6.5 reserves that for menus, popovers, dialogs and toasts.
// The card sits in the page, it does not float above it.

import type { ReactElement, ReactNode } from 'react';

import styles from './panel-card.module.css';

export function PanelCard({ children }: { children: ReactNode }) {
  return <div className={styles.card}>{children}</div>;
}

/**
 * One module: a `--control-h-lg` head — untinted, on the card's own surface;
 * the label tier is what marks it as a head, and `--surface-panel-head` has
 * been deleted from `tokens.css` (see `panel-card.module.css`) — with an
 * optional trailing control, then a body. Modules are separated by a hairline
 * — they are siblings inside one object, which is the boundary ladder's step ②
 * and the reason they do not each get their own card (§6.5 forbids nesting a
 * card inside a card).
 */
export function PanelModule({ title, action, children }: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className={styles.module}>
      <div className={styles.head}>
        <h2 className={styles.title}>{title}</h2>
        {action}
      </div>
      <div className={styles.body}>{children}</div>
    </section>
  );
}

/**
 * The one control a module head may carry, trailing the title.
 *
 * It lives here rather than in each page because the head belongs to the
 * module: two pages hand-rolling the same 20px glyph button is how the second
 * one drifts. `label` is the accessible name *and* the hover title — §4.4 is
 * explicit that a tooltip may not stand in for the accessible name, so both
 * come from one argument and cannot disagree.
 */
export function PanelAction({ label, onClick, children }: {
  label: string;
  onClick: () => void;
  children: ReactElement;
}) {
  return (
    <button
      type="button"
      data-nc-role="icon"
      className={styles.action}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      {children}
    </button>
  );
}

/**
 * §5.3 — an unbuilt region shows the *shape* of what is coming and one short
 * sentence, with no module path, no slice name and no apology.
 */
export function PanelEmpty({ children }: { children: string }) {
  return <p className={styles.empty}>{children}</p>;
}
