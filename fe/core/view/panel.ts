// The panel view vocabulary, and the traversal panel renderers will run.
//
// #1234 — the wave page's panel exists twice in the DOM (a desktop panel card
// and a mobile drill-down list), and the two are hand-copied: the same fact is
// spelled differently on each side, or is missing from one. The fix is not a
// shared component — the two surfaces compose their rows differently and must
// keep doing so — but a shared *derivation*: one platform-independent view
// model, and renderers that are faithful projections of it.
//
// This module holds the vocabulary of that view model plus `paintModule`, the
// traversal S1b's renderers are meant to share. Nothing calls it yet outside
// `panel.test.ts`; see its docstring for what that does and does not buy.
// Everything here is data and pure functions; `core` may not
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
   *  into the row's status marker attribute, unchanged. The attribute is
   *  deliberately not named here: it is `data-nc-task-status` today, and S1b
   *  renames it to `data-nc-status` when the panel is rewritten. */
  token: string;
  /**
   * The finished, readable string — a text obligation: the canonical phrase
   * **both surfaces are to consume**, so that neither has to word the status
   * for itself. Offering one canonical phrase is all a type can do; that each
   * renderer actually uses it, rather than wording its own, is enforced by
   * S1b's projection check, not here.
   *
   * The claim is about the row text and stops there: an **accessible name** may
   * be more than the row text, and today the desktop's is — the dot's
   * `aria-label` is `Status: ${phrase}` (`public.tsx:730`) while its `title` is
   * the bare phrase (`:731`). That `Status: ` prefix is **renderer chrome and
   * is not in this field**, deliberately: it is a labelling decision about one
   * platform's graphic, not wording about the run.
   *
   * Nothing here stops a renderer wording its chrome differently from the
   * other. In the version of `view-characterization.test.tsx` *before* this
   * slice's fix, that gap was wider still: a mutation that moved the `Status: `
   * prefix *into* this field stayed green there. The suite's current exact
   * assertion on the dot's `title` does catch that particular mutation; chrome
   * consistency in general is still S1b's painter contract, not this type's.
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
 * omitted, and is required so that an **explicitly declared** unsupported
 * action must state why.
 *
 * That is the whole of today's guarantee, and it is narrower than "an omission
 * cannot be silent". This type constrains only what a painter *writes in its
 * `action` table*; nothing reads that table yet (`paintModule` does not — see
 * its docstring), so a renderer can still just not paint an action and no `why`
 * is ever demanded of it. Omission-is-not-silent becomes true when S1b's
 * `paintPanel` consults the table.
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
 * Paint one module: the traversal both of S1b's renderers are to go through,
 * and which will then be their single shared one. Today it has no renderer
 * caller at all (see the three open gaps below).
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
