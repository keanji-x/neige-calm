// The panel view vocabulary and the traversal panel renderers share: one platform-independent
// view model, and renderers that supply leaf constructors through `RowPainter<T>`.

import type { ActivityState } from '../domain/activity.js';

/** A short word next to a row's name: `kernel-owned`, `Withdrawn`, … */
export type RowBadge = Readonly<{ id: string; text: string; struck: boolean }>;

export type RowStatus = Readonly<{
  /** The bare status word; the renderer writes it into `MARKER.status` unchanged. */
  token: string;
  /** The canonical readable phrase both surfaces consume; renderer chrome such as a `Status: ` prefix is not part of it. */
  phrase: string;
}>;

/**
 * One control a row offers, carrying its own wording. The wording is derived per row, not per
 * kind, and the three fields are separate channels: `title` must not touch the accessible name (WCAG 2.5.3).
 */
export type RowAction = Readonly<{
  /** The accessible name for a control that has no visible text of its own;
   *  null when the control has visible text — a second `aria-label` would
   *  override that visible text (WCAG 2.5.3). */
  label: string | null;
  /** The pointer tooltip. null = offer no hint. */
  hint: string | null;
  /** The non-visible description for the focused control; separate from `hint` because assistive tech does not inherit `title`. */
  description: string | null;
}> & (
  | Readonly<{ kind: 'reveal-block'; blockId: string }>
  | Readonly<{ kind: 'open-card'; cardId: string }>
  | Readonly<{ kind: 'delete-card'; cardId: string }>
);

export type PanelRow = Readonly<{
  id: string;
  title: string;
  kind: string | null;
  badges: readonly RowBadge[];
  status: RowStatus | null;
  /** The kernel's activity verdict for this row's card, from `TrackActivity.cards` alone; `null` when the kernel listed none. */
  activity: ActivityState | null;
  /** An ordered set, not named slots: placement is the painter's call, membership and order are the view model's. */
  actions: readonly RowAction[];
}>;

export type RowModuleView = Readonly<{
  key: 'cards' | 'tasks';
  title: string;
  rows: readonly PanelRow[];
  /** What the module says when it has no rows. */
  empty: string;
}>;

export type TrackPageView = Readonly<{ rowModules: readonly RowModuleView[] }>;

/**
 * Whether a renderer offers an action at all; `paintModule` filters a row's actions by this before
 * calling `row()`. Support may depend on a host prop, so a painter is rebuilt per render.
 */
export type ActionSupport =
  | Readonly<{ supported: true }>
  | Readonly<{ supported: false; why: string }>;

/** A renderer's leaf constructors. `T` is whatever the platform builds — `ReactNode` on both surfaces, a string in tests. */
export type RowPainter<T> = Readonly<{
  row(row: PanelRow): T;
  module(parts: Readonly<{ key: RowModuleView['key']; title: string; children: readonly T[] }>): T;
  empty(text: string): T;
  action: Readonly<Record<RowAction['kind'], ActionSupport>>;
}>;

/**
 * Paint one module. The empty state is exclusive: `empty()` runs only for zero rows, `row()` only
 * otherwise, and each row is handed only the actions the painter's capability table supports.
 */
export function paintModule<T>(painter: RowPainter<T>, module: RowModuleView): T {
  const paintRow = (row: PanelRow): T => painter.row({
    ...row,
    actions: row.actions.filter((action) => painter.action[action.kind].supported),
  });
  const children: readonly T[] = module.rows.length === 0
    ? [painter.empty(module.empty)]
    : module.rows.map(paintRow);
  return painter.module({ key: module.key, title: module.title, children });
}

/** Paint the whole panel: every row module, in the view model's order. Mobile drills into one module at a time and calls `paintModule` directly. */
export function paintPanel<T>(painter: RowPainter<T>, view: TrackPageView): readonly T[] {
  return view.rowModules.map((module) => paintModule(painter, module));
}

/**
 * The DOM marker attribute names, read by the checker, tests and the production painters.
 * Constants, not types: a type would let two different strings both satisfy it. `ui/**` and
 * stylesheets cannot import from here and spell the names themselves.
 */
export const MARKER = Object.freeze({
  /** Bijection anchor for a row module. Carries no text obligation. */
  module: 'data-nc-module',
  /** Bijection anchor, scope and subtraction boundary for a row. Carries no
   *  text obligation of its own — every field has its own carrier. */
  row: 'data-nc-row',
  /** Value is the badge id; the element's text domain is the badge text. */
  badge: 'data-nc-badge',
  /** A host annotation, not a content marker. Not `data-nc-action`: that is the global button-styling
   *  protocol in `styles/base.css` with a frozen four-value vocabulary. */
  action: 'data-nc-row-action',
  /** Value is `RowStatus.token`; the element's `title` is `RowStatus.phrase`. */
  status: 'data-nc-status',
  /** A content marker whose value names which field the element carries; its
   *  text domain equals that field exactly. See `FIELD`. */
  field: 'data-nc-field',
} as const);

/** The permitted values of `MARKER.field` — closed; any other value is `field-domain`. */
export const FIELD = Object.freeze({
  /** `PanelRow.title`. Exactly one per row. */
  title: 'title',
  /** `PanelRow.kind`. Exactly one per row when non-null, zero when null. */
  kind: 'kind',
  /** `RowModuleView.title`. Exactly one per module. */
  moduleTitle: 'module-title',
  /** `RowModuleView.empty`. Exactly one in a module with zero rows, zero
   *  otherwise. */
  empty: 'empty',
} as const);
