/**
 * §6.4 — the page header, shared by all four routes.
 *
 * Three rows, of which only the title row is mandatory. Which rows exist is a
 * *rule*, not a per-page choice: row 1 renders the entity's domain ancestor (a
 * wave's is its cove; a cove has none; Today is the root), and row 3 renders a
 * machine identifier if the entity has one.
 *
 * `--header-h` is therefore computed per page from the rows actually rendered,
 * not a constant. Writing it as a fixed 92 was the old bug: only one of the four
 * routes is 92, and `scroll-margin-block-start` with the wrong value either
 * hides a focused row behind the sticky header or leaves 60px of gap under it.
 */

import type { ReactNode, RefObject } from 'react';

import styles from './page-header.module.css';

export type PageHeaderProps = Readonly<{
  /** Row 1. Omitted entirely when the entity has no domain ancestor. */
  breadcrumb?: ReactNode;
  /** Row 2, always present, and the home of the single page-title element. */
  title: ReactNode;
  /** Row 2, after the title: counts and badges. */
  meta?: ReactNode;
  /** Row 2, after the spring: at most one primary, then destructive, then clock. */
  actions?: ReactNode;
  /** Row 3. A cwd, an id, a branch — mono, because the font says "literal". */
  identity?: ReactNode;
  /**
   * Where the title sits.
   *
   * `page` starts it at the page's own inset — right for a route whose main
   * column *is* the page, like Settings.
   *
   * `document` starts it at the prose column instead. The document routes
   * centre their prose inside the main column, and the title is that
   * document's first line: left at the page inset it hangs 106px out to the
   * side of every word below it. Actions are unaffected — they belong to the
   * page's trailing edge, not to the document.
   */
  align?: 'page' | 'document';
}>;

export function PageHeader({ breadcrumb, title, meta, actions, identity, align = 'page' }: PageHeaderProps) {
  const rows = 1 + (breadcrumb === undefined ? 0 : 1) + (identity === undefined ? 0 : 1);
  return (
    <header
      className={styles.header}
      data-nc-header-rows={String(rows) as '1' | '2' | '3'}
      data-nc-align={align}
    >
      {breadcrumb !== undefined && <div className={styles.crumbRow}>{breadcrumb}</div>}
      <div className={styles.titleRow}>
        {title}
        {meta}
        <span className={styles.spring} />
        {actions}
      </div>
      {identity !== undefined && <div className={styles.identityRow}>{identity}</div>}
    </header>
  );
}

/**
 * The one page-title element. Exactly one per route, and the four routes render
 * it identically — which is why it steps *out* of the hierarchy competition
 * rather than winning it (§3.3).
 *
 * `tabIndex={-1}` exists only so CR-8 can focus it after a delete removes the
 * button that opened the dialog; it stays out of the Tab order, and `base.css`
 * suppresses the ring for programmatic focus.
 */
export function PageTitle({ children, titleRef }: {
  children: ReactNode;
  titleRef?: RefObject<HTMLHeadingElement | null>;
}) {
  return (
    <h1 ref={titleRef} className={styles.title} data-nc-page-title tabIndex={-1}>
      {children}
    </h1>
  );
}

/** §6.14 — at most two levels, because a domain ancestor is only ever one deep. */
export function Breadcrumb({ ancestor, current, onNavigate, onNavigateCurrent, backLabel, onBack }: {
  ancestor: string;
  current?: ReactNode;
  onNavigate: () => void;
  /**
   * Supply this when the last crumb is still an *ancestor* rather than the page
   * you are on — the wave page's trail is Today / cove, and the page itself is
   * the title underneath. A trailing crumb that names somewhere else and cannot
   * be clicked is a dead end; only the crumb for the current page is inert.
   */
  onNavigateCurrent?: () => void;
  backLabel?: string;
  onBack?: () => void;
}) {
  return (
    <nav className={styles.crumbs} aria-label="Breadcrumb">
      {onBack !== undefined && (
        <button type="button" data-nc-role="icon" className={styles.back}
          aria-label={backLabel ?? 'Back'} onClick={onBack}>←</button>
      )}
      <button type="button" data-nc-role="row" className={styles.crumbLink} onClick={onNavigate}>
        {ancestor}
      </button>
      {current !== undefined && (
        <>
          {/* A rendered separator glyph, not a character inside a label: the
              "no `/` in rendered strings" gate measures text nodes, and this is
              a decorative element with its own registered exemption. */}
          <span className={styles.crumbSep} aria-hidden="true">/</span>
          {onNavigateCurrent === undefined
            ? <span className={styles.crumbCurrent}>{current}</span>
            : (
              <button type="button" data-nc-role="row" className={`${styles.crumbLink} ${styles.crumbCurrent}`} onClick={onNavigateCurrent}>
                {current}
              </button>
            )}
        </>
      )}
    </nav>
  );
}
