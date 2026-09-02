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
  /**
   * The finished, readable string — a text obligation.
   *
   * **The row's own visible wording lives in `core`**, not in either renderer,
   * so the two cannot word the status differently. That claim is about the row
   * text and stops there: an **accessible name** may be more than the row text,
   * and today the desktop's is — the dot's `aria-label` is
   * `Status: ${phrase}` (`public.tsx:730`) while its `title` is the bare phrase
   * (`:731`). That `Status: ` prefix is **renderer chrome and is not in this
   * field**, deliberately: it is a labelling decision about one platform's
   * graphic, not wording about the run.
   *
   * Nothing here stops a renderer wording its chrome differently from the
   * other, and `view-characterization.test.tsx` has been shown not to catch it
   * (a mutation that moved the `Status: ` prefix *into* this field stayed
   * green). Chrome consistency is S1b's painter contract, not this type's.
   */
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

/**
 * Whether a renderer offers an action at all. `why` is the reason it is
 * omitted, and is required so an omission cannot be silent.
 *
 * **Support is not necessarily a platform constant.** The desktop's real
 * condition for `delete-card` is `onDeleteCard !== undefined`
 * (`public.tsx:514`) — a *host prop*, which can differ between two renders of
 * the same platform, not a fact about the platform. Since `RowPainter.action`
 * is a fixed table, expressing that means **rebuilding the painter per render**
 * with the support bound to the prop. S1b's desktop painter must do exactly
 * that: a painter that hard-codes `delete-card` as supported grows a delete
 * button on a page whose host never passed a handler, which is a control that
 * does not exist today.
 */
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
 * **The empty state is exclusive** (#1234 §5.20): `empty()` is called when the
 * module has zero rows and *only* then, and `row()` is called for every row and
 * only when there is at least one. Leaving that to each renderer is exactly how
 * the two surfaces drifted in the first place — one of them can print an empty
 * line under a populated list, or omit the empty text entirely, and nothing
 * notices.
 *
 * **That exclusivity is structural only within this function, and this slice
 * does not yet make anyone enter it.** Three gaps are open on purpose, and all
 * three are S1b's `paintPanel` plus its projection check:
 *
 *  - Nothing forces a renderer to *call* this at all. Both surfaces still
 *    compose their modules by hand; "both renderers are on the one branch" is a
 *    goal, not a fact this file can enforce.
 *  - `deriveWavePageView` returns a `rowModules` **sequence** and there is no
 *    traversal over it here. A renderer can paint cards, skip tasks, or reorder
 *    them, while `wave-page.ts` claims the order is part of the view model.
 *    The obligation to walk the sequence has no carrier yet.
 *  - `RowPainter.action` is required by the type and **never read** by this
 *    function. It is a declaration today; the code that consults it — and so
 *    makes an unsupported action's `why` mean anything — arrives with the
 *    painters.
 *
 * Adding `paintPanel` here would be out of S1a's scope; this note is the
 * accounting for what the docstring above does *not* buy yet.
 */
export function paintModule<T>(painter: RowPainter<T>, module: RowModuleView): T {
  const children: readonly T[] = module.rows.length === 0
    ? [painter.empty(module.empty)]
    : module.rows.map((row) => painter.row(row));
  return painter.module({ key: module.key, title: module.title, children });
}
