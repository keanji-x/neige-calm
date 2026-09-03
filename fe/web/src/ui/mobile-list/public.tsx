import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { useId, useLayoutEffect, useRef, type ReactNode } from 'react';

import { MobileHeader } from '../mobile-header/public.tsx';
import styles from './mobile-list.module.css';

/*
 * #1234 S1b-4a / S1b-4b — six projection marker channels, plus two channels
 * that are not markers: the pointer tooltip and the row's accessible
 * description.
 *
 * The mobile painter has to put the panel's projection markers on elements this
 * primitive owns: the drill-down page's container, its heading (by way of
 * `MobileHeader`), a row's `<li>`, the **visible title span inside that `<li>`**,
 * the empty line's text carrier, and — since S1b-4b's Task row — the row's own
 * action host, which is that same `<li>`. A single rest-prop spread on the outermost
 * element reaches none of the inner ones — and `MobileListPage` alone needs two
 * different targets — so each channel is its own named, **opt-in** prop. This is
 * the same shape `ui/panel-card` took for the desktop (S1b-3b), for the same
 * reason.
 *
 * **The other two channels are wording, not markers, and they are three
 * different things.** A row's visible `title` is its name; `hint` is the pointer
 * tooltip (`RowAction.hint`, forwarded as the `<li>`'s `title` attribute); and
 * `accessibleDescription` is text a screen-reader user gets *on top of* the
 * name. None substitutes for another — WCAG 2.5.3 is why the tooltip may not
 * become the name. The three do not land the same way either: the visible
 * `title` is handed to Astryx as its `label` and is rendered into a span inside
 * the `<li>`, the tooltip rides a rest prop onto the `<li>` itself, and the
 * description is the only one that has to be written **imperatively**, because
 * the element it must reach is a control Astryx generates and hands no props to.
 * See `MobileListItem`'s docstrings for each.
 *
 * **Opt-in, and that is load-bearing.** `app/shell`'s area and page lists and
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
  /**
   * #1234 S1b-4b — the row's **accessible description**: text a screen-reader
   * user gets on top of the row's name, never instead of it.
   *
   * **Why a description and not a longer name.** A Task row shows its status in
   * the meta lane, which Astryx renders in `endContent` — a *sibling* of the
   * invisible button, not a child of it. So the button's accessible name is the
   * task key alone and `failed — not a git repository` is not in it anywhere,
   * while the desktop's reveal button (which encloses its status dot) names the
   * whole reason. That is missing information, not a naming style, and
   * `aria-describedby` closes it without touching the visible name.
   *
   * **The attribute is set imperatively, and that is not a shortcut.** Astryx's
   * `Item` spreads rest props onto the root `<li>` only — its `BaseProps` accepts
   * `aria-*`, but every one of them lands on the container. The invisible
   * `<button>` it generates takes no props from the outside at all, and an
   * `aria-describedby` on the `<li>` never reaches the focused control. So the
   * text carrier is rendered declaratively (a clipped span in `endContent`, id
   * from `useId` — no random source, because production is plain-http LAN where
   * `crypto.randomUUID` does not exist) and the reference is attached to
   * whichever control Astryx actually generated, falling back to the `<li>` for
   * a row that is not interactive. `public.test.tsx` asserts the attribute is on
   * the **button** rather than the `<li>`, so the day Astryx moves that control
   * this goes red instead of going quiet — and the effect below counts the
   * direct-child controls rather than taking the first, so the day Astryx
   * generates *two* it says so in development instead of quietly describing one
   * of them.
   *
   * Absent unless passed: neither the span nor the attribute exists otherwise.
   */
  accessibleDescription?: string;
  /** #1234 — the value of `data-nc-row` on the root `<li>`. */
  rowMarker?: string;
  /**
   * #1234 — the value of `data-nc-row-action` on the root `<li>`.
   *
   * **A sixth channel, and it shares an element with `rowMarker` on purpose.**
   * On this surface the whole row is the tappable control (§3.5), so the `<li>`
   * is both the row carrier and the action host. That is legal precisely
   * because `data-nc-row-action` is a *host annotation* rather than a content
   * marker: `tools/projection/public.ts`'s `CONTENT_MARKERS` omits it, and its
   * `owned()` counts the container itself so the co-hosted shape reads as one
   * row with one action rather than a row with none.
   */
  rowActionMarker?: string;
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

  const rootRef = useRef<HTMLLIElement | null>(null);
  const descriptionId = `${useId()}mobile-row-description`;
  const interactive = onSelect !== undefined;
  /*
   * A **layout** effect, and the tier is the point rather than a habit.
   *
   * The carrier span is rendered declaratively, so it is in the DOM the moment
   * the commit lands; the IDREF that points at it is written from here. A
   * passive `useEffect` is not guaranteed to run before the browser paints — it
   * is scheduled after the commit, and under concurrent rendering it can be
   * deferred further — so a **paintable window may open** in which the two
   * halves disagree: a row that just gained a description has the span but no
   * `aria-describedby`, and a row that just lost one has an attribute still
   * naming a span React already removed. A layout effect runs inside the commit,
   * synchronously before paint, so the pair is never left observable apart.
   *
   * The tier's usual cost is a forced synchronous layout; nothing here reads
   * geometry or computed style, so that particular cost is absent — the work
   * still blocks paint, but it is a `querySelectorAll` and an attribute write.
   * And the app has no SSR path (a single `createRoot`, no `hydrateRoot`), so
   * there is no server render to warn.
   */
  useLayoutEffect(() => {
    if (accessibleDescription === undefined) return undefined;
    const root = rootRef.current;
    if (root === null) return undefined;
    /*
     * The control Astryx generated for an interactive row — the element that
     * takes focus, and therefore the only one an `aria-describedby` reaches a
     * reader through. A non-interactive row has none, and the container is the
     * honest fallback rather than a silently dropped description.
     *
     * **A count is checked, and `querySelector` alone was not enough.** Taking
     * the first match is safe only while there is exactly one; the day Astryx's
     * `Item` renders a second direct-child control — a trailing action button
     * beside the invisible one, say — the first match still gets described, the
     * second focusable control gets nothing, and *every existing assertion here
     * stays green*, because they all read the first one too. The structural
     * change that this file's tests do catch is a control moving deeper (the
     * selector then finds none and the description lands on the `<li>` while a
     * button is focusable); the multiplicity change is the one they cannot see.
     *
     * **What this count covers, and only this.** It is a development diagnostic
     * over one narrow shape: a row that was *given* an `accessibleDescription`,
     * counted as **direct-child `button` / `a` of the `<li>`**, at the instant
     * this effect runs. It is not an accessibility audit of the row, and four
     * things are outside it by construction:
     *
     *  - a row with no `accessibleDescription` at all — the effect returns above
     *    and counts nothing;
     *  - a second control **nested** inside a wrapper Astryx renders (a
     *    `startContent` node is wrapped in a `<span>`, and an extra action moved
     *    into an existing end-content wrapper would be invisible here);
     *  - a direct-child `input` / `select` / `textarea` / `[tabindex]` — the
     *    selector names two tag names, not "focusable";
     *  - a control that appears *after* this effect has run while the deps
     *    (`accessibleDescription`, `descriptionId`, `interactive`) are unchanged.
     *
     * So a green run here means "the one shape this checks still holds", not
     * "the row has exactly one focusable host".
     *
     * **Loud in development, degrading in production.** The count firing is an
     * accessibility hole that is invisible from the DOM — nothing about a
     * described first button says a second one was skipped — so development and
     * CI throw, which is where an Astryx upgrade is looked at. Production does
     * not: this component renders inside the wave route, the app configures no
     * `errorComponent`, and a throw from a layout effect is caught by the
     * router's global `CatchBoundary`, which replaces the whole match — shell
     * and navigation included — with a bare error page. Trading the entire
     * surface for an alarm about a dependency pinned at `0.1.3` is the wrong
     * side of that call, so production logs and falls back to the selection this
     * guard replaced: the first control if there is one, the `<li>` otherwise.
     */
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
    /* Identical to `interactive ? controls[0] : root` whenever the count above
       held — an inert row has no control — and the degraded choice when it did
       not. */
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
