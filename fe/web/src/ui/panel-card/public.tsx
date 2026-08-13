// The panel column's one card, and the modules stacked inside it.
//
// Every route has the same skeleton now: an unbounded report document in the
// main column, and **one** rounded card top-right holding two modules — the
// route's own (Today's calendar, a cove's wave list, a wave's cards) and the
// conversation list, which is the same on all three.
//
// This overrides §6.5's "面板列里的面板不画自己的边框". That rule was written
// for a panel column holding one thing, where the 24px gutter already said
// where the panel began and a border would have said it twice. It now holds two
// modules of different function, and the gutter cannot say they are one object
// — only the container can. §6.5's own decision ladder allows exactly this:
// step ③ (different function → change surface) plus step ② (a hairline), and
// the two are permitted *together* here because `--surface-card` and `--bg` sit
// 2.8 L apart, inside the ladder's own "< 3.0 L" exemption.
//
// Still no shadow: §6.5 reserves that for menus, popovers, dialogs and toasts.
// The card sits in the page, it does not float above it.

import type { ReactNode } from 'react';

import styles from './panel-card.module.css';

export function PanelCard({ children }: { children: ReactNode }) {
  return <div className={styles.card}>{children}</div>;
}

/**
 * One module: a `--control-h-lg` head on the card surface with an
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
  children: string;
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
