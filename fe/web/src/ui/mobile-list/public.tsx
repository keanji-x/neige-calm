import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import type { ReactNode } from 'react';

import { MobileHeader } from '../mobile-header/public.tsx';
import styles from './mobile-list.module.css';

/*
 * #1234 S1b-4a — five projection marker channels, and one tooltip channel.
 *
 * The mobile painter has to put the panel's projection markers on elements this
 * primitive owns: the drill-down page's container, its heading (by way of
 * `MobileHeader`), a row's `<li>`, the **visible title span inside that `<li>`**,
 * and the empty line's text carrier. A single rest-prop spread on the outermost
 * element reaches none of the inner ones — and `MobileListPage` alone needs two
 * different targets — so each channel is its own named, **opt-in** prop. This is
 * the same shape `ui/panel-card` took for the desktop (S1b-3b), for the same
 * reason.
 *
 * **Opt-in, and that is load-bearing.** `app/shell`'s cove and page lists and
 * this page's Outline / Conversations pages render through these same
 * primitives and are not row modules. Marking unconditionally would put
 * `data-nc-module` / `data-nc-row` elements in trees whose view model has none,
 * and the bijection would go red against a faithful painter. Every channel below
 * is absent unless a value is passed, and `public.test.tsx` pins both
 * directions.
 *
 * **Why the attribute names are spelled here rather than imported.**
 * `.dependency-cruiser.cjs`'s `ui-only-core-type-whitelist` lets `ui/**` import
 * only `core/types/{ids,a11y}.ts` and `core/state/types.ts`, so this file cannot
 * read `core/view/panel.ts`'s `MARKER` / `FIELD`. The props therefore take the
 * marker *value* as a bare `string`, and the attribute names are literals here.
 * What stops that second spelling drifting is not review:
 * `features/wave/page/mobile-projection.test.tsx` runs `checkProjectionIn` —
 * whose selectors are built from `MARKER` — over the real rendered mobile panel,
 * so a literal that disagreed would leave the checker finding no marker at all.
 */

function motionClass(motion: 'none' | 'forward' | 'back'): string {
  if (motion === 'forward') return styles.pageForward;
  if (motion === 'back') return styles.pageBack;
  return '';
}

export function MobileListPage({
  title, backLabel, onBack, motion = 'none', children, moduleMarker, titleFieldMarker,
}: Readonly<{
  title: string;
  backLabel?: string;
  onBack?: () => void;
  motion?: 'none' | 'forward' | 'back';
  children: ReactNode;
  /** #1234 — the value of `data-nc-module` on this page's container. Omit on a
   *  page that is not a row module. */
  moduleMarker?: string;
  /** #1234 — the value of `data-nc-field` on this page's heading, which
   *  `MobileHeader` owns. A second target, so a second channel. */
  titleFieldMarker?: string;
}>) {
  return (
    <div
      className={`${styles.page} ${motionClass(motion)}`}
      {...(moduleMarker === undefined ? {} : { 'data-nc-module': moduleMarker })}
    >
      <MobileHeader
        title={title}
        backLabel={backLabel}
        onBack={onBack}
        titleFieldMarker={titleFieldMarker}
      />
      <div className={styles.content}>{children}</div>
    </div>
  );
}

export function MobileList({ title, children }: Readonly<{ title?: string; children: ReactNode }>) {
  return (
    <section className={styles.section}>
      {title !== undefined && <h3>{title}</h3>}
      <AstryxList className={styles.list} density="balanced">{children}</AstryxList>
    </section>
  );
}

export function MobileListEmpty({ children, fieldMarker }: Readonly<{
  children: ReactNode;
  /** #1234 — the value of `data-nc-field` on the text's own carrier. The `<p>`
   *  *is* the carrier: it holds the empty sentence and nothing else, which is
   *  what the projection's leaf rule requires. */
  fieldMarker?: string;
}>) {
  return (
    <li>
      <p
        className={styles.empty}
        {...(fieldMarker === undefined ? {} : { 'data-nc-field': fieldMarker })}
      >{children}</p>
    </li>
  );
}

export function MobileListItem({
  title, meta, startContent, ariaLabel, nested = false, titleVariant = 'interface', onSelect,
  hint, rowMarker, titleFieldMarker,
}: Readonly<{
  title: string;
  meta?: ReactNode;
  startContent?: ReactNode;
  ariaLabel?: string;
  /** Visually nests a second-level row while keeping one flat, touch-friendly list. */
  nested?: boolean;
  /** Document titles use the report's editorial typography; utility rows stay sans. */
  titleVariant?: 'interface' | 'document';
  /**
   * #1234 — **optional, and its absence is a shape, not a default.** A row with
   * no `onSelect` must reach the DOM with **no `onClick` at all**: Astryx's
   * `Item` computes `isInteractive = onClick != null`, so the tempting
   * `onClick={() => onSelect?.()}` would still generate the invisible button,
   * the pointer cursor and the hover — a control that does nothing. The mobile
   * Cards row is exactly that case (§3.6: the two card actions are not offered
   * on this viewport), so the row is a plain, non-interactive `<li>`.
   */
  onSelect?: () => void;
  /**
   * #1234 — the pointer tooltip, forwarded as `title` on the root `<li>`.
   *
   * **Not the visible `title` prop above**, which is this row's *name*. A
   * `RowAction` carries `label` and `hint` as two deliberately separate channels
   * (WCAG 2.5.3), and until this prop existed the mobile row could express only
   * one of them. Absent unless passed — the projection asserts that a `null`
   * hint leaves no attribute behind.
   */
  hint?: string;
  /** #1234 — the value of `data-nc-row` on the root `<li>`. */
  rowMarker?: string;
  /** #1234 — the value of `data-nc-field` on the **visible title span**, which
   *  lives inside this primitive. It cannot go on the `<li>`: that already
   *  carries `data-nc-row`, and one element may carry at most one content
   *  marker. */
  titleFieldMarker?: string;
}>) {
  const metaLabel = typeof meta === 'string' || typeof meta === 'number' ? String(meta) : null;
  /* `title` is not in Astryx's `BaseProps` (it omits it deliberately), so the
     attribute travels as a spread rather than a named JSX prop; it still lands
     on the root `<li>` through `ListItem`'s rest props. */
  const hintAttribute: Readonly<Record<string, string>> = hint === undefined ? {} : { title: hint };
  return (
    <AstryxListItem
      className={`${styles.item} ${onSelect === undefined ? styles.itemStatic : ''}` +
        `${nested ? ` ${styles.itemNested}` : ''}`}
      label={(
        <span
          className={`${styles.itemTitle} ${titleVariant === 'document' ? styles.itemTitleDocument : ''}`}
          {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}
        >
          {title}
        </span>
      )}
      startContent={startContent}
      {...(onSelect === undefined ? {} : { onClick: () => onSelect() })}
      aria-label={ariaLabel ?? (metaLabel === null ? undefined : `${title}, ${metaLabel}`)}
      endContent={meta === undefined ? undefined : <span className={styles.meta}>{meta}</span>}
      {...(rowMarker === undefined ? {} : { 'data-nc-row': rowMarker })}
      {...hintAttribute}
    />
  );
}
