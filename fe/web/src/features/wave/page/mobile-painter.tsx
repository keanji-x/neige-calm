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
// **Scope: Cards.** The mobile Tasks page and the mobile navigation sequence are
// still hand-composed in `public.tsx` and arrive in S1b-4b. Handed a Tasks
// module, the pending row leaf therefore throws while the module is finished,
// rather than guessing — a Task row's composition (a status carrier in the meta
// lane, `reveal-block` on the row root) is not this file's yet, and silently
// painting a Cards row for it would be a projection fault reported far from its
// cause.

import type { ReactNode } from 'react';

import { FIELD, MARKER, paintModule } from '../../../../../core/view/panel.ts';
import type {
  PanelRow, RowBadge, RowModuleView, RowPainter,
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
   * this surface supports, and the Task row that hosts it lands in S1b-4b —
   * the capability table already says so, because support is a statement about
   * the surface rather than about which module has been wired yet.
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

    row: (row) => ({
      slot: 'row',
      paint: (moduleKey) => {
        if (moduleKey !== 'cards') {
          throw new Error(`the mobile painter has no ${moduleKey} row yet (#1234 S1b-4b)`);
        }
        return cardRow(row);
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
