// The desktop track panel, painted from `core/view`'s panel view model (#1234).
//
// This is the first production renderer on the `paintModule` / `paintPanel`
// branch. Before it, the traversal and the projection checker existed and were
// exercised only by synthetic painters, while `public.tsx` hand-composed the two
// row modules — the shape §6.10 calls the false sense of safety, and the shape
// this slice exists to remove.
//
// **The DOM here is the page's, moved, not redesigned.** Every class name,
// `data-nc-role`, `aria-label` and `title` the desktop panel rendered before is
// reproduced verbatim, because a stack of suites is measuring it:
// `task-row.browser.test.tsx` hit-tests the task row's invisible reveal sheet
// and the trailing lane, `public.test.tsx` reads the reveal button's accessible
// name as one sentence, and `public.contract.test.tsx` counts the buttons inside
// `[data-nc-task-inventory]`. What is new is the marker layer, and nothing else.
//
// **Why `T` is not `ReactNode`.** `RowPainter.row` is handed a `PanelRow` and
// nothing else — no module context — but the desktop's two modules compose a row
// differently in ways no shared shape covers: a Cards row is a body button whose
// meta lane holds the kind tag and the ownership badge, with the delete × as a
// sibling; a Task row is a reveal button holding the key, the declaration and
// the status dot, with the kind as a sibling. Deciding between them by sniffing
// the row (`does it carry a reveal-block action?`) would make this file correct
// only by an invariant of `track-page.ts` that nothing states, which is the
// mistake `track-page.ts`'s own docstrings warn about twice. So a row or empty
// leaf is a *pending* node instead: it is finished once the module it belongs
// to says which module that is. `slot` is the second half of the same problem —
// `paintModule` calls `empty()` exactly when a module has no rows, but the `T`
// it hands back does not say so, and the desktop wraps rows in a `<ul>` and the
// empty line in nothing. `DesktopLeaf` is therefore a tagged union rather than
// one shape: a module leaf is already finished, and saying so is what keeps the
// top level from looking a module's key up a second time (see its docstring).
//
// **Symbol references only.** The DOM here came out of `track/page/public.tsx`,
// and several docstrings in this issue cited the lines it used to occupy. Every
// one of them was stale by the next edit; they name functions and components
// now.

import type { ReactNode } from 'react';

import { FIELD, MARKER, paintPanel } from '../../../../../core/view/panel.ts';
import type {
  ActionSupport, PanelRow, RowAction, RowBadge, RowModuleView, RowPainter, RowStatus, TrackPageView,
} from '../../../../../core/view/panel.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import { PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import styles from './page.module.css';

/**
 * A row or an empty line, painted as far as it can be here: it still needs to
 * know which module it lands in, and `module()` is the only thing that knows.
 * `paint` is called from there and nowhere else.
 */
type PendingLeaf = Readonly<{
  slot: 'row' | 'empty';
  paint: (moduleKey: RowModuleView['key']) => ReactNode;
}>;

/**
 * A module, which needs nothing further: `module()` was handed `parts.key` and
 * resolved its own children against it there.
 */
type ModuleLeaf = Readonly<{ slot: 'module'; node: ReactNode }>;

/**
 * **A tagged union, and the tag is the difference that matters.** A module leaf
 * is *finished*; a row or empty leaf is not. Giving both halves the same
 * `paint(moduleKey)` signature is what made the top level look the key up again
 * by position (`view.rowModules[i].key`) — re-deriving, off the view's *order*,
 * a fact `module()` had already captured from `parts.key`. Correct only because
 * `paintPanel` is order-preserving, and one fact bound to two places. The union
 * says instead that only one of the two ever needs a key, and that one never
 * reaches the top level.
 */
export type DesktopLeaf = PendingLeaf | ModuleLeaf;

export type DesktopPainterDeps = Readonly<{
  onOpenCard?: (cardId: string) => void;
  onOpenTask?: (blockId: string) => void;
  onDeleteCard?: (cardId: string) => void;
  /** The Cards module head's `+`, composed by `app/router`. A router-composed
   *  slot, not view-model content (§3.2), so it travels as an opaque node. */
  cardsAction?: ReactNode;
}>;

/** One marker attribute, spelled from `MARKER` / `FIELD` so no name is retyped
 *  here. Written as a spread because the attribute name is a value. */
const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

/**
 * What a painter needs from one of a row's actions: the payload id it carries,
 * and its two wording channels.
 *
 * The lookup returns `null` when the action is absent — and it is absent for two
 * different reasons that the painter deliberately does not distinguish: the view
 * model never derived it (an undeletable card has no `delete-card`), or
 * `paintModule` filtered it out against this painter's capability table. Both
 * mean the same thing here: draw no control, and therefore no marker.
 */
type Control = Readonly<{ id: string; label: string | null; hint: string | null }>;

function control(row: PanelRow, kind: RowAction['kind']): Control | null {
  for (const action of row.actions) {
    if (action.kind !== kind) continue;
    return {
      id: action.kind === 'reveal-block' ? action.blockId : action.cardId,
      label: action.label,
      hint: action.hint,
    };
  }
  return null;
}

/** `label` / `hint` are exact on both sides: `null` must emit no attribute at
 *  all, because a second accessible name overrides a control's visible text
 *  (WCAG 2.5.3) and the projection asserts the absence. */
function wording(action: Control): Readonly<Record<string, string>> {
  return {
    ...(action.label === null ? {} : { 'aria-label': action.label }),
    ...(action.hint === null ? {} : { title: action.hint }),
  };
}

/**
 * The status dot, moved verbatim out of the track page's former inline Tasks
 * module (S1b-3b).
 *
 * Three carriers, unchanged. `data-nc-status` holds the **bare token**, which is
 * what the stylesheet keys colour off — folding the kernel's reason into it
 * would leave a failed row uncoloured. `title` holds the phrase a pointer gets.
 * `aria-label` holds `Status: ${phrase}`, and that prefix is **painter chrome**:
 * it is deliberately not part of `RowStatus.phrase` (see `core/view/panel.ts`),
 * and the projection reads the other two carriers only, which is what leaves the
 * prefix this renderer's decision to make.
 *
 * It sits *inside* the reveal button on purpose — the row's whole job is to
 * reveal the block, and being a DOM child is what makes the click bubble, so the
 * dot owns its hover without owning the click. Its trailing position is CSS.
 */
function statusDot(status: RowStatus): ReactNode {
  return (
    <span
      key="status"
      className={styles.taskDot}
      {...mark(MARKER.status, status.token)}
      role="img"
      aria-label={`Status: ${status.phrase}`}
      title={status.phrase}
    />
  );
}

function cardBadge(badge: RowBadge): ReactNode {
  return (
    <span key={badge.id} className={styles.kernelOwned} {...mark(MARKER.badge, badge.id)}>
      {badge.text}
    </span>
  );
}

/**
 * A Cards row — the `<li>` the track page used to spell inline under its `Cards`
 * `PanelModule`, moved here by S1b-3b.
 *
 * The delete is a **sibling** of the row button, never a child: a `<button>`
 * inside a `<button>` is dropped by every HTML parser, and
 * `public.contract.test.tsx` holds that line for the whole page.
 *
 * The former `removable` condition was `onDeleteCard !== undefined &&
 * card.deletable`. Both halves survive, in different places: `card.deletable`
 * decides whether `track-page.ts` derives a `delete-card` action at all, and
 * `onDeleteCard !== undefined` is this painter's capability table, which
 * `paintModule` applies before `row` is called. So the control appears exactly
 * when the filtered action does — which is also what makes the marker layer able
 * to see a painter that grew a delete button the host never asked for.
 */
function cardRow(row: PanelRow, deps: DesktopPainterDeps): ReactNode {
  const open = control(row, 'open-card');
  const remove = control(row, 'delete-card');
  return (
    <li key={row.id} className={styles.cardItem} {...mark(MARKER.row, row.id)}>
      <button
        type="button"
        className={`${styles.cardRow} ${remove !== null ? styles.cardRowRemovable : ''}`}
        {...(open === null ? {} : { ...mark(MARKER.action, 'open-card'), ...wording(open) })}
        onClick={open === null ? undefined : () => deps.onOpenCard?.(open.id)}
      >
        <span className={styles.cardKind} {...mark(MARKER.field, FIELD.title)}>{row.title}</span>
        <span className={styles.cardMeta}>
          {/* Only when a title took the name slot — an untitled card is already
              showing its kind there, and printing it twice is noise. The
              condition is now the view model's (`row.kind === null`), which is
              the same rule read from one place instead of two. */}
          {row.kind !== null && (
            <span className={styles.cardKindTag} {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>
          )}
          {row.badges.map(cardBadge)}
        </span>
      </button>
      {remove !== null && (
        <button
          type="button"
          data-nc-role="icon"
          className={styles.cardRemove}
          {...mark(MARKER.action, 'delete-card')}
          {...wording(remove)}
          onClick={() => deps.onDeleteCard?.(remove.id)}
        >
          <Icon name="close" size="sm" />
        </button>
      )}
    </li>
  );
}

/**
 * A Task row — the `<li>` the track page used to spell inline under its `Tasks`
 * `PanelModule`, moved here by S1b-3b.
 *
 * **Two controls in one row, and the second is a sibling.** The row used to be
 * one `<button>` that decided for the reader which of two landings it took; a
 * dispatched task then had no way back to its declaration at all. The reveal
 * button always reveals the block; the *kind* is the worker-card affordance, and
 * only when there is a card to open. A `<button>` may not nest inside a
 * `<button>`, which is the mechanical reason the row is a plain `<li>`.
 *
 * **"The row reveals the block" is a CSS fact, not DOM containment.**
 * `.taskReveal::before` paints an invisible sheet over the whole `<li>`, and the
 * two things that stay above it are the kind button and the status dot. That
 * claim is hit-tested in `task-row.browser.test.tsx`; jsdom reports this same
 * tree whether the sheet covers the row or nothing, which is why the geometry is
 * not asserted anywhere near here.
 *
 * **The kind carries two markers, and that is legal.** It is the `kind` field's
 * carrier *and*, when there is a card, the `open-card` host — and
 * `data-nc-row-action` is a host annotation, not a content marker, so the
 * one-content-marker-per-element rule is not engaged. The declaration badge and
 * the status dot sit inside the reveal button for the same reason: an action
 * host does not own the field text underneath it.
 *
 * The kind is drawn from `row.kind`, and the *control* form from the presence of
 * the `open-card` action — which the derivation produces exactly when
 * `kind !== null && workerCardId !== null`, the page's own former two nested
 * tests. If a view model ever offered `open-card` on a row with no kind, this
 * paints no host for it and the projection says `action-sequence`: unreachable
 * today, and loud rather than silent if it stops being.
 */
function taskRow(row: PanelRow, deps: DesktopPainterDeps): ReactNode {
  const reveal = control(row, 'reveal-block');
  const open = control(row, 'open-card');
  return (
    <li key={row.id} className={styles.taskRow} {...mark(MARKER.row, row.id)}>
      <button
        type="button"
        className={styles.taskReveal}
        {...(reveal === null ? {} : { ...mark(MARKER.action, 'reveal-block'), ...wording(reveal) })}
        onClick={reveal === null ? undefined : () => deps.onOpenTask?.(reveal.id)}
      >
        {/* Mono: the key is the literal other reports and the kernel address
            this task by (§2.2). */}
        <span className={styles.taskKey} {...mark(MARKER.field, FIELD.title)}>{row.title}</span>
        {/* The declaration's own word — `Not ready`, `Withdrawn`, `Unreadable` —
            and nothing else: the run is the dot. `struck` is the withdrawal, and
            it is the view model's fact now rather than a second reading of
            `task.state` here. */}
        {row.badges.map((badge) => (
          <span
            key={badge.id}
            className={badge.struck ? styles.taskWithdrawn : styles.taskNote}
            {...mark(MARKER.badge, badge.id)}
            title={badge.id.startsWith('pending-reason:') ? badge.text : undefined}
          >{badge.text}</span>
        ))}
        {row.status !== null && statusDot(row.status)}
      </button>
      {/* The kind is a word either way — what changes is whether it is a
          control. `title` describes the destination without touching the
          accessible name, which stays the visible word (WCAG 2.5.3). */}
      {row.kind !== null && (open === null
        ? <span className={styles.taskKind} {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>
        : (
          <button
            type="button"
            className={styles.taskKindButton}
            {...mark(MARKER.field, FIELD.kind)}
            {...mark(MARKER.action, 'open-card')}
            {...wording(open)}
            onClick={() => deps.onOpenCard?.(open.id)}
          >
            {row.kind}
          </button>
        ))}
    </li>
  );
}

/**
 * The desktop panel's painter, rebuilt per render.
 *
 * **The capability table is computed here, from the host props**, and that is
 * the point of the factory: `delete-card`'s support is `onDeleteCard !==
 * undefined`, which is a fact about *this render*, not about the desktop. A
 * painter that hard-coded it as supported would grow a delete control on a page
 * whose host passed no handler — a control that does not exist today.
 *
 * `open-card` and `reveal-block` are unconditionally supported, and that is not
 * an oversight: the page draws both controls regardless of whether a callback
 * arrived (`onOpenCard?.(…)` is optional at the call site, and
 * `public.contract.test.tsx` counts five buttons in a task list rendered with no
 * `onOpenCard` at all). Binding support to the callback would delete controls
 * that exist today. Whether a supported action reaches a live handler is not a
 * projection obligation (§6.3).
 */
export function makeDesktopPainter(deps: DesktopPainterDeps): RowPainter<DesktopLeaf> {
  const deleteSupport: ActionSupport = deps.onDeleteCard === undefined
    ? { supported: false, why: 'the host passed no onDeleteCard, so this render offers no delete' }
    : { supported: true };

  return {
    action: {
      'reveal-block': { supported: true },
      'open-card': { supported: true },
      'delete-card': deleteSupport,
    },

    row: (row) => ({
      slot: 'row',
      paint: (moduleKey) => (moduleKey === 'cards' ? cardRow(row, deps) : taskRow(row, deps)),
    }),

    empty: (text) => ({
      slot: 'empty',
      paint: () => <PanelEmpty key="empty" fieldMarker={FIELD.empty}>{text}</PanelEmpty>,
    }),

    module: (parts) => {
      const children = parts.children.map((leaf) => finish(leaf, parts.key));
      const rows = parts.children.every((leaf) => leaf.slot === 'row');
      return {
        slot: 'module',
        node: (
          <PanelModule
            key={parts.key}
            title={parts.title}
            action={parts.key === 'cards' ? deps.cardsAction : undefined}
            moduleMarker={parts.key}
            titleFieldMarker={FIELD.moduleTitle}
          >
            {!rows ? children
              : parts.key === 'cards'
                ? <ul className={styles.cards} data-nc-card-inventory="">{children}</ul>
                : <ul className={styles.tasks} data-nc-task-inventory="">{children}</ul>}
          </PanelModule>
        ),
      };
    },
  };
}

/**
 * Resolve one of a module's children against the module it landed in.
 *
 * `paintModule` builds a module's children out of `row()` and `empty()` only,
 * so the module arm is unreachable — and it throws rather than rendering
 * nothing, because a module nested inside a module would be a traversal that
 * has changed shape underneath this file, not a leaf worth silently dropping.
 */
function finish(leaf: DesktopLeaf, moduleKey: RowModuleView['key']): ReactNode {
  if (leaf.slot === 'module') {
    throw new Error(`paintModule handed the ${moduleKey} module a module leaf as a child`);
  }
  return leaf.paint(moduleKey);
}

/**
 * `paintPanel`, unwrapped into nodes the page can render.
 *
 * The traversal is `core/view`'s, and every leaf it returns is a module that
 * `module()` already finished against its own `parts.key`. So this unwraps and
 * does nothing else: it reads no key, and therefore cannot re-bind one to the
 * view's order.
 */
export function paintDesktopPanel(
  painter: RowPainter<DesktopLeaf>,
  view: TrackPageView,
): readonly ReactNode[] {
  return paintPanel(painter, view).map((leaf) => {
    if (leaf.slot !== 'module') {
      throw new Error(`paintPanel returned a ${leaf.slot} leaf where a module was due`);
    }
    return leaf.node;
  });
}
