// The mobile wave panel, painted from `core/view`'s panel view model (#1234).
//
// The desktop went through `paintPanel` in S1b-3b; this is the second renderer,
// and the one the whole issue is about — the mobile drill-down list is where the
// two surfaces had drifted apart (an untitled card printing its kind twice, no
// `kernel-owned`, a wording of its own for every state).
//
// **Mobile does not call `paintPanel`.** The desktop panel card lays both row
// modules side by side in one tree, so one traversal produces the whole thing.
// Mobile drills into one module at a time, so the module *sequence* is a
// navigation structure rather than a DOM sequence (Δ2): each page calls
// `paintModule` once, through `paintMobileModule` below. That is also why the
// projection is run with a one-module list here and a two-module one on the
// desktop — the checker takes the expected modules as an argument for exactly
// this reason.
//
// **Two card actions are deliberately unsupported** (§3.6, owner). What is not
// offered is *those two actions*, not the Cards module: the row still exists,
// still prints its title, its kind and `kernel-owned` from the same derivation
// the desktop reads, and what it lacks is the tappable wrapper. A negative
// assertion alone ("mobile has no delete") would be satisfied by the whole Cards
// list disappearing; the domain and bijection checks are its necessary partner,
// and that is what `mobile-projection.test.tsx` runs.
//
// **Scope: both row modules** since S1b-4b. The Task row lands here with its
// status carrier in the meta lane and `reveal-block` on the row root, and
// `public.tsx` composes neither list itself any more. The module *sequence* is
// the drill-down menu, and since S1b-4b that too is derived — `rowModules.map`,
// not two written-out entries (Δ2) — so a module the derivation gains, loses or
// reorders moves the mobile navigation with it.
//
// **The two surfaces still word a status differently, and only in their chrome.**
// The desktop draws a dot, which is a graphic and therefore needs a name; this
// one prints the token. Both write the same `data-nc-status` and the same
// `title`, which is the part the projection reads — see `statusWord`.

import type { ReactNode } from 'react';

import { FIELD, MARKER, paintModule } from '../../../../../core/view/panel.ts';
import type {
  PanelRow, RowAction, RowBadge, RowModuleView, RowPainter, RowStatus,
} from '../../../../../core/view/panel.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../../ui/mobile-list/public.tsx';
import styles from './page.module.css';

/**
 * A row or an empty line, painted as far as it can be here: it still needs to
 * know which module it lands in, and `module()` is the only thing that knows.
 * The desktop painter's `DesktopLeaf` has the same shape and the same reason —
 * see its docstring for why a module leaf is a separate arm rather than a third
 * `paint` implementation.
 */
type PendingLeaf = Readonly<{
  slot: 'row' | 'empty';
  paint: (moduleKey: RowModuleView['key']) => ReactNode;
}>;

/** A module, which needs nothing further: `module()` was handed `parts.key`. */
type ModuleLeaf = Readonly<{ slot: 'module'; node: ReactNode }>;

export type MobileLeaf = PendingLeaf | ModuleLeaf;

export type MobilePainterDeps = Readonly<{
  /**
   * Reveal a task's block in the document. `reveal-block` is the one action
   * this surface supports, and since S1b-4b the Task row hosts it on the row
   * root itself.
   */
  onOpenTask?: (blockId: string) => void;
  /** The mobile page chrome: what the back button says, where it goes, and how
   *  the page animates in. Not view-model content — it is the drill-down's own
   *  navigation, which is why it travels in the factory's closure rather than
   *  through `RowPainter`'s signature (§3.5). */
  backLabel?: string;
  onBack?: () => void;
  motion?: 'none' | 'forward' | 'back';
}>;

/** One marker attribute, spelled from `MARKER` / `FIELD` so no name is retyped
 *  here. Written as a spread because the attribute name is a value. */
const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

function cardBadge(badge: RowBadge): ReactNode {
  return (
    <span key={badge.id} {...mark(MARKER.badge, badge.id)}>{badge.text}</span>
  );
}

/**
 * A Task row's declaration badge.
 *
 * **`struck` is drawn here, and it has to be**: `RowBadge.struck` is a formal
 * field of the derivation with **no carrier in the projection framework at all**
 * (`tools/projection/public.ts`'s standing list says so in as many words —
 * `checkBadges` reads a badge's id, its position and its text and nothing else).
 * A painter that ignored it would be green under every violation code. So the
 * desktop pins it with a class assertion outside the projection
 * (`public.test.tsx`, `taskWithdrawn`), and this surface takes the same shape:
 * `mobile-painter.test.tsx` asserts the struck class on a withdrawn declaration
 * and its absence on an ordinary one.
 */
function taskBadge(badge: RowBadge): ReactNode {
  return (
    <span
      key={badge.id}
      className={badge.struck ? styles.mobileRowStruck : undefined}
      {...mark(MARKER.badge, badge.id)}
    >{badge.text}</span>
  );
}

/**
 * The status, in the meta lane.
 *
 * **The word, not the desktop's dot.** The two carriers the projection reads are
 * the same on both surfaces — `data-nc-status` holds the bare token (which is
 * what the stylesheet keys colour off, so folding the kernel's reason into it
 * would leave a failed row uncoloured) and `title` holds the phrase. What
 * differs is the chrome around them, and deliberately: the desktop dot is a
 * graphic with no text, so it needs `role="img"` and an `aria-label` of
 * `Status: ${phrase}` to be reachable at all. Here the element **prints the
 * token**, so it is reachable already, and adding an `aria-label` would override
 * that visible word rather than add to it (the same WCAG 2.5.3 rule the action
 * wording follows). A dot in a drill-down list would also be a colour with no
 * legend on the one surface that has no hover.
 *
 * **The token on screen is not the whole story a reader is owed.** This element
 * is `endContent`, which Astryx lays out as a **sibling** of the invisible button
 * — so the row's accessible name is the task key and the kernel's reason is
 * nowhere in it, while the desktop's reveal button *encloses* its status dot and
 * therefore names `Status: failed — wave … is not a git repository` in full.
 * That asymmetry is missing information, not a wording choice, so `taskRow`
 * hands the same `phrase` to `MobileListItem`'s `accessibleDescription` channel:
 * the name stays the visible key, and the reason arrives as the control's
 * description. It is emitted whenever there is a status — including when
 * `phrase === token`, where it is the *only* way the status reaches a reader who
 * is on the button, since the visible word is outside it.
 *
 * `RowStatus.phrase` is not this file's to word — `core/view/wave-page.ts` owns
 * it, and that is the whole reason the mobile surface stopped re-wording state.
 */
function statusWord(status: RowStatus): ReactNode {
  return (
    <span key="status" {...mark(MARKER.status, status.token)} title={status.phrase}>
      {status.token}
    </span>
  );
}

/**
 * The `reveal-block` action, or null when the view model offered none. The
 * lookup is by kind rather than by position: `paintModule` has already filtered
 * the array against the capability table, so what survives is not the
 * derivation's own sequence.
 *
 * **All three of the action's fields come back, `label` included.** It would be
 * true *today* that `deriveWavePageView` words a Task row's reveal with
 * `label: null` — and a painter that read only `blockId` and `hint` would be
 * green for exactly that reason, which is a fact about the derivation rather
 * than about this file. `RowAction` carries `label` as a channel of its own and
 * the projection checks it on both sides (`action-label`), so the painter
 * consumes it and lets the view model decide whether there is one.
 */
function reveal(
  row: PanelRow,
): Readonly<{ blockId: string; label: string | null; hint: string | null }> | null {
  for (const action of row.actions) {
    if (action.kind !== 'reveal-block') continue;
    return { blockId: action.blockId, label: action.label, hint: action.hint };
  }
  return null;
}

/**
 * A Tasks row.
 *
 * **The row root is both the row carrier and the action host**, and that pair is
 * legal: `data-nc-row-action` is a host annotation, not a content marker, so the
 * one-content-marker-per-element rule is not engaged and
 * `tools/projection/public.ts`'s `owned()` counts the container itself. This is
 * the first production shape to take that path — S1b-2 wrote the fix for it
 * before anything used it — so `mobile-projection.test.tsx` asserts the co-hosted
 * `<li>` explicitly rather than leaving it implied by a green run.
 *
 * **What the row stopped deciding for itself.** It used to re-word `task.state`
 * into `Ready` / `Not ready` / `Withdrawn` / `Unreadable` here, which is how this
 * surface disagreed with the desktop about the same task. Both of
 * `deriveReportTasks`' rules now arrive through the derivation instead: `ready`
 * carries no word at all (a column in which every row has a word is a column
 * nobody reads), and a `status` supersedes the readiness word (a dispatched task
 * printing `Not ready` beside `running` is a row arguing with itself). D8 —
 * owner-approved, and the one visible behaviour change in this slice.
 *
 * **`label` and `hint` are the action's two wording channels, and both are
 * consumed.** `label` becomes the host's `aria-label` when the view model offers
 * one and is **omitted entirely** when it is null — the row has visible text, and
 * a fabricated second accessible name would override it (WCAG 2.5.3). The
 * derivation words the Task reveal with `label: null` today, so the emitted
 * shape is "no attribute"; that is the view model's call, not a rule this file
 * writes down. `hint` travels through the primitive's `hint` channel to the
 * `<li>`'s `title` — not through its visible `title` prop, which is the row's
 * name.
 *
 * The `open-card` action the derivation offers on a worker-card task never
 * reaches here: this painter declares it unsupported, so `paintModule` filters it
 * out before `row()` is called. The kind is still printed — what is not offered
 * is the action, not the fact (§3.6).
 */
function taskRow(row: PanelRow, deps: MobilePainterDeps): ReactNode {
  const action = reveal(row);
  const meta: readonly ReactNode[] = [
    ...row.badges.map(taskBadge),
    ...(row.status === null ? [] : [statusWord(row.status)]),
    ...(row.kind === null
      ? []
      : [<span key="kind" {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>]),
  ];
  return (
    <MobileListItem
      key={row.id}
      title={row.title}
      rowMarker={row.id}
      titleFieldMarker={FIELD.title}
      {...(row.status === null ? {} : { accessibleDescription: row.status.phrase })}
      {...(action === null ? {} : {
        rowActionMarker: 'reveal-block' satisfies RowAction['kind'],
        onSelect: () => deps.onOpenTask?.(action.blockId),
        ...(action.label === null ? {} : { ariaLabel: action.label }),
        ...(action.hint === null ? {} : { hint: action.hint }),
      })}
      meta={meta.length === 0 ? undefined : <span className={styles.mobileRowMeta}>{meta}</span>}
    />
  );
}

/**
 * A Cards row.
 *
 * **Three things the mobile row did not have before**, all of them now read
 * from the one derivation instead of being decided here:
 *
 *  - the kind is printed **only when a title took the name slot**
 *    (`row.kind !== null`). The mobile list used to pass `meta={card.kind}`
 *    unconditionally, so an untitled card — whose derived name *is* its kind —
 *    printed the same word twice;
 *  - `kernel-owned` appears, because badges are the view model's and the
 *    desktop's own lane is no longer the only one that reads them;
 *  - the row is **not a control**. Both card actions are unsupported here, so
 *    `paintModule` hands this function a row with no actions at all and no
 *    `onSelect` is passed — which, since S1b-4a's primitive change, means no
 *    `onClick` reaches Astryx and no invisible button is generated.
 *
 * **The title carrier is the span inside the primitive, not the `<li>`.** The
 * `<li>` carries `data-nc-row`, and one element may carry at most one content
 * marker; that rule is why `MobileListItem` needed a named channel for its
 * visible title rather than a rest-prop spread.
 *
 * The kind and the badges are **separate elements in the meta lane**, each its
 * own leaf carrier. Wrapping them in one span would give the projection a
 * carrier whose text is `terminalkernel-owned`.
 */
function cardRow(row: PanelRow): ReactNode {
  const meta: readonly ReactNode[] = [
    ...(row.kind === null
      ? []
      : [<span key="kind" {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>]),
    ...row.badges.map(cardBadge),
  ];
  return (
    <MobileListItem
      key={row.id}
      title={row.title}
      rowMarker={row.id}
      titleFieldMarker={FIELD.title}
      meta={meta.length === 0 ? undefined : <span className={styles.mobileRowMeta}>{meta}</span>}
    />
  );
}

/**
 * The mobile panel's painter, rebuilt per render.
 *
 * **The capability table is the deliberate inconsistency, written down** (D1 /
 * D7, owner). `why` is required of an unsupported action precisely so that this
 * is a decision on the record rather than a surface that quietly does less.
 *
 * Unlike the desktop's, no entry here is bound to a host prop: not offering the
 * card actions is a fact about this viewport, not about this render.
 */
export function makeMobilePainter(deps: MobilePainterDeps): RowPainter<MobileLeaf> {
  return {
    action: {
      'reveal-block': { supported: true },
      'open-card': {
        supported: false,
        why: 'cards render poorly at this viewport, so opening a card is not offered on mobile (owner, #1234)',
      },
      'delete-card': {
        supported: false,
        why: 'the same call, kept simple: card operations are not offered on mobile (owner, #1234)',
      },
    },

    /* Dispatch on the module, never on the row's shape: deciding by sniffing
       (`does it carry a reveal-block action?`) would make this file correct only
       by an invariant of `wave-page.ts` that nothing states. A third module key
       still throws rather than falling back to a Cards row — a projection fault
       reported far from its cause is the thing the throw exists to prevent. */
    row: (row) => ({
      slot: 'row',
      paint: (moduleKey) => {
        if (moduleKey === 'cards') return cardRow(row);
        if (moduleKey === 'tasks') return taskRow(row, deps);
        const unknown: never = moduleKey;
        throw new Error(`the mobile painter has no ${String(unknown)} row`);
      },
    }),

    empty: (text) => ({
      slot: 'empty',
      paint: () => <MobileListEmpty key="empty" fieldMarker={FIELD.empty}>{text}</MobileListEmpty>,
    }),

    module: (parts) => ({
      slot: 'module',
      node: (
        <MobileListPage
          key={parts.key}
          title={parts.title}
          backLabel={deps.backLabel}
          onBack={deps.onBack}
          motion={deps.motion}
          moduleMarker={parts.key}
          titleFieldMarker={FIELD.moduleTitle}
        >
          <MobileList>{parts.children.map((leaf) => finish(leaf, parts.key))}</MobileList>
        </MobileListPage>
      ),
    }),
  };
}

/**
 * Resolve one of a module's children against the module it landed in.
 *
 * `paintModule` builds a module's children out of `row()` and `empty()` only,
 * so the module arm is unreachable — and it throws rather than rendering
 * nothing, because a module nested inside a module would be a traversal that
 * has changed shape underneath this file.
 */
function finish(leaf: MobileLeaf, moduleKey: RowModuleView['key']): ReactNode {
  if (leaf.slot === 'module') {
    throw new Error(`paintModule handed the ${moduleKey} module a module leaf as a child`);
  }
  return leaf.paint(moduleKey);
}

/**
 * `paintModule`, unwrapped into the one page the mobile surface shows.
 *
 * **One module, not the panel.** This is the mobile counterpart of the desktop's
 * `paintDesktopPanel`, and the difference in arity is the whole of Δ2: the
 * caller picks the module the reader drilled into, and the module sequence is
 * carried by the navigation menu instead of by a traversal.
 */
export function paintMobileModule(
  painter: RowPainter<MobileLeaf>,
  module: RowModuleView,
): ReactNode {
  const leaf = paintModule(painter, module);
  if (leaf.slot !== 'module') {
    throw new Error(`paintModule returned a ${leaf.slot} leaf where a module was due`);
  }
  return leaf.node;
}
