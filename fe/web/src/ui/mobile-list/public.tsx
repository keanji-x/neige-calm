import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { useId, useLayoutEffect, useRef, type ReactNode } from 'react';

import { MobileHeader } from '../mobile-header/public.tsx';
import styles from './mobile-list.module.css';

/* Each projection marker channel is its own opt-in prop: a rest-prop spread reaches only the outermost element, and unconditional marking would put row markers in trees whose view model has none. The attribute names are literals because `ui/**` may not import `core/view/panel.ts`. */

export function MobileListPage({
  title, backLabel, onBack, actions, children, moduleMarker, titleFieldMarker,
}: Readonly<{
  title: string;
  backLabel?: string;
  onBack?: () => void;
  actions?: ReactNode;
  children: ReactNode;
  /** The value of `data-nc-module` on this page's container; omit on a page that is not a row module. */
  moduleMarker?: string;
  /** The value of `data-nc-field` on this page's heading, which `MobileHeader` owns. */
  titleFieldMarker?: string;
}>) {
  return (
    <div
      className={styles.page}
      {...(moduleMarker === undefined ? {} : { 'data-nc-module': moduleMarker })}
    >
      <MobileHeader
        title={title}
        backLabel={backLabel}
        onBack={onBack}
        actions={actions}
        titleFieldMarker={titleFieldMarker}
      />
      <div className={styles.content}>{children}</div>
    </div>
  );
}

export function MobileListGroup({ label, children }: Readonly<{ label: string; children: ReactNode }>) {
  return <div role="group" aria-label={label} className={styles.group}>{children}</div>;
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
  /** The value of `data-nc-field` on the `<p>`, which holds the empty sentence and nothing else. */
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
  hint, accessibleDescription, rowMarker, rowActionMarker, titleFieldMarker,
}: Readonly<{
  title: string;
  meta?: ReactNode;
  startContent?: ReactNode;
  ariaLabel?: string;
  /** Visually nests a second-level row while keeping one flat, touch-friendly list. */
  nested?: boolean;
  /** Document titles use the report's editorial typography; utility rows stay sans. */
  titleVariant?: 'interface' | 'document';
  /** Optional, and its absence is a shape: Astryx's `Item` computes `isInteractive = onClick != null`, so a `() => onSelect?.()` wrapper would still generate a control that does nothing. */
  onSelect?: () => void;
  /** The pointer tooltip, forwarded as `title` on the root `<li>`; not the visible `title` prop, which is the row's name (WCAG 2.5.3). */
  hint?: string;
  /** Text a screen-reader user gets on top of the row's name. Set imperatively: Astryx hands the generated `<button>` no props, so an `aria-describedby` on the `<li>` never reaches the focused control. The id comes from `useId`, not `crypto.randomUUID`, which is absent on plain-http LAN. */
  accessibleDescription?: string;
  /** The value of `data-nc-row` on the root `<li>`. */
  rowMarker?: string;
  /** The value of `data-nc-row-action` on the root `<li>`; it shares the element with `rowMarker` because it is a host annotation, not a content marker. */
  rowActionMarker?: string;
  /** The value of `data-nc-field` on the visible title span; the `<li>` already carries `data-nc-row` and may hold only one content marker. */
  titleFieldMarker?: string;
}>) {
  const metaLabel = typeof meta === 'string' || typeof meta === 'number' ? String(meta) : null;
  /* `title` is not in Astryx's `BaseProps`, so it travels as a spread onto the root `<li>`. */
  const hintAttribute: Readonly<Record<string, string>> = hint === undefined ? {} : { title: hint };

  const rootRef = useRef<HTMLLIElement | null>(null);
  const descriptionId = `${useId()}mobile-row-description`;
  const interactive = onSelect !== undefined;
  /* A layout effect: the carrier span is committed declaratively and the IDREF written here, and a passive effect could leave a paintable window where the two disagree. */
  useLayoutEffect(() => {
    if (accessibleDescription === undefined) return undefined;
    const root = rootRef.current;
    if (root === null) return undefined;
    /* Direct-child controls are counted rather than taking the first: a second one would go silently undescribed. Development throws; production logs and falls back, since a throw from a layout effect reaches the router's global `CatchBoundary` and replaces the whole match. */
    const controls = root.querySelectorAll(':scope > button, :scope > a');
    const expected = interactive ? 1 : 0;
    if (controls.length !== expected) {
      const message =
        `MobileListItem: ${interactive ? 'an interactive' : 'a non-interactive'} row expects `
        + `${expected} control as a direct child of its <li>, but the list primitive rendered `
        + `${controls.length}. This counts only direct-child <button>/<a> on a row that was `
        + 'given an accessibleDescription, so it says nothing about controls nested deeper, '
        + 'about other focusable elements, or about ones that appear later; the row\'s '
        + 'accessible description has no single host to attach to. This component has to be '
        + 'updated for the new markup.';
      if (import.meta.env.DEV) throw new Error(message);
      console.error(message);
    }
    const host = controls[0] ?? root;
    host.setAttribute('aria-describedby', descriptionId);
    return () => { host.removeAttribute('aria-describedby'); };
  }, [accessibleDescription, descriptionId, interactive]);

  const metaSlot = meta === undefined ? null : <span className={styles.meta}>{meta}</span>;
  const descriptionSlot = accessibleDescription === undefined
    ? null
    : <span className={styles.srOnly} id={descriptionId}>{accessibleDescription}</span>;

  return (
    <AstryxListItem
      ref={rootRef}
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
      endContent={metaSlot === null && descriptionSlot === null
        ? undefined
        : <>{metaSlot}{descriptionSlot}</>}
      {...(rowMarker === undefined ? {} : { 'data-nc-row': rowMarker })}
      {...(rowActionMarker === undefined ? {} : { 'data-nc-row-action': rowActionMarker })}
      {...hintAttribute}
    />
  );
}
