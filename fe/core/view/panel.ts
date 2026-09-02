// The panel view vocabulary, and the one traversal every panel renderer runs.
//
// #1234 — the wave page's panel exists twice in the DOM (a desktop panel card
// and a mobile drill-down list), and the two are hand-copied: the same fact is
// spelled differently on each side, or is missing from one. The fix is not a
// shared component — the two surfaces compose their rows differently and must
// keep doing so — but a shared *derivation*: one platform-independent view
// model, and renderers that are faithful projections of it.
//
// This module holds the vocabulary of that view model plus `paintModule`, the
// only traversal. Everything here is data and pure functions; `core` may not
// import React or touch the DOM (`fe/core/AGENTS.md`), so a renderer supplies
// its own leaf constructors through `RowPainter<T>`.
//
// Deliberately *not* here (see the #1234 design, §3.2): module-head slots
// (`conversations` / `backlinks` / the `+` menu) are `ReactNode`s the router
// composes and a platform-independent derivation cannot construct, so they stay
// ordinary props on the page component. The view model describes only what is
// derivable.

/** A short word next to a row's name: `kernel-owned`, `Withdrawn`, … */
export type RowBadge = Readonly<{ id: string; text: string; struck: boolean }>;

export type RowStatus = Readonly<{
  /** The bare status word — a structural obligation: the renderer writes it
   *  into the `data-nc-status` attribute, unchanged. */
  token: string;
  /** The finished, readable string — a text obligation. The wording lives in
   *  `core`, not in either renderer, so the two cannot word it differently. */
  phrase: string;
}>;

export type RowAction =
  | Readonly<{ kind: 'reveal-block'; blockId: string }>
  | Readonly<{ kind: 'open-card'; cardId: string }>
  | Readonly<{ kind: 'delete-card'; cardId: string }>;

export type PanelRow = Readonly<{
  id: string;
  title: string;
  kind: string | null;
  badges: readonly RowBadge[];
  status: RowStatus | null;
  /** An ordered set, not named slots — where an action is placed is the
   *  painter's call; *which* actions exist and in what order is the view
   *  model's, and is checked as a sequence. */
  actions: readonly RowAction[];
}>;

export type RowModuleView = Readonly<{
  key: 'cards' | 'tasks';
  title: string;
  rows: readonly PanelRow[];
  /** What the module says when it has no rows. */
  empty: string;
}>;

export type WavePageView = Readonly<{ rowModules: readonly RowModuleView[] }>;

/** Whether a renderer offers an action at all. `why` is the reason a platform
 *  deliberately omits it, and is required so an omission cannot be silent. */
export type ActionSupport =
  | Readonly<{ supported: true }>
  | Readonly<{ supported: false; why: string }>;

/**
 * A renderer's leaf constructors. `T` is whatever the platform builds —
 * `ReactNode` on both current surfaces, a string in tests.
 *
 * Composition inside a row stays with `row()`: the desktop task row nests its
 * key, declaration and status dot inside one reveal button while the kind is a
 * sibling, and the mobile row is a single tappable item. Those orders are
 * mutually exclusive, so there is nothing to share below the row.
 */
export type RowPainter<T> = Readonly<{
  row(row: PanelRow): T;
  module(parts: Readonly<{ key: RowModuleView['key']; title: string; children: readonly T[] }>): T;
  empty(text: string): T;
  action: Readonly<Record<RowAction['kind'], ActionSupport>>;
}>;

/**
 * Paint one module: the single traversal both renderers go through.
 *
 * **The empty state is exclusive, and that is this function's whole reason to
 * exist** (#1234 §5.20). `empty()` is called when the module has zero rows and
 * *only* then, and `row()` is called for every row and only when there is at
 * least one. Leaving that to each renderer is exactly how the two surfaces
 * drifted in the first place — one of them can print an empty line under a
 * populated list, or omit the empty text entirely, and nothing notices. Here it
 * is structural: there is one branch, and both renderers are on it.
 */
export function paintModule<T>(painter: RowPainter<T>, module: RowModuleView): T {
  const children: readonly T[] = module.rows.length === 0
    ? [painter.empty(module.empty)]
    : module.rows.map((row) => painter.row(row));
  return painter.module({ key: module.key, title: module.title, children });
}
