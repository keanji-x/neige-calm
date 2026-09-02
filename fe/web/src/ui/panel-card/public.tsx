import type { ReactElement, ReactNode } from 'react';

import styles from './panel-card.module.css';

/*
 * #1234 S1b-3b — three projection marker channels.
 *
 * The desktop painter has to put the panel's projection markers on elements
 * this primitive owns: the module's `<section>`, its `<h2>`, and the empty
 * line's `<p>`. A single rest-prop spread on the outermost element cannot do
 * that — `PanelModule` alone needs two different targets — so each channel is
 * its own named, **opt-in** prop.
 *
 * **Opt-in, and that is load-bearing.** The wave page renders `Referenced by`
 * and `Conversations` through `PanelModule` too, and those are not row modules.
 * Marking unconditionally would put four `data-nc-module` elements in a tree
 * whose view model has two, and the module-layer bijection would go red against
 * a faithful painter. Every channel below is absent unless a value is passed,
 * and `panel-card.test.tsx` pins both directions.
 *
 * **Why the attribute names are spelled here rather than imported.**
 * `.dependency-cruiser.cjs`'s `ui-only-core-type-whitelist` lets `ui/**` import
 * only `core/types/{ids,a11y}.ts` and `core/state/types.ts`, so this file cannot
 * read `core/view/panel.ts`'s `MARKER` / `FIELD`. The props therefore take the
 * marker *value* as a bare `string`, and the names are literals here. That is a
 * second spelling of `data-nc-module` / `data-nc-field`, and the thing that
 * stops it drifting from `MARKER` is not review: `desktop-projection.test.tsx`
 * runs `checkProjectionIn` — whose selectors are built from `MARKER` — over the
 * real rendered page, so a literal that disagreed with the table would leave the
 * checker finding no module marker at all.
 */

export function PanelCard({ children }: { children: ReactNode }) {
  return <div className={styles.card}>{children}</div>;
}

export function PanelModule({ title, action, children, moduleMarker, titleFieldMarker }: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
  /** #1234 — the value of `data-nc-module` on this module's `<section>`. Omit on
   *  a module that is not part of the panel's view model. */
  moduleMarker?: string;
  /** #1234 — the value of `data-nc-field` on this module's `<h2>`. */
  titleFieldMarker?: string;
}) {
  return (
    <section
      className={styles.module}
      {...(moduleMarker === undefined ? {} : { 'data-nc-module': moduleMarker })}
    >
      <div className={styles.head}>
        <h2
          className={styles.title}
          {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}
        >{title}</h2>
        {action}
      </div>
      <div className={styles.body}>{children}</div>
    </section>
  );
}

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

export function PanelEmpty({ children, fieldMarker }: {
  children: string;
  /** #1234 — the value of `data-nc-field` on the text's own carrier. The `<p>`
   *  *is* the carrier: it holds the empty sentence and nothing else, which is
   *  what the projection's leaf rule requires. */
  fieldMarker?: string;
}) {
  return (
    <p
      className={styles.empty}
      {...(fieldMarker === undefined ? {} : { 'data-nc-field': fieldMarker })}
    >{children}</p>
  );
}
