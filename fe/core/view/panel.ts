// The panel view vocabulary, and the traversal panel renderers will run.
//
// #1234 — the wave page's panel exists twice in the DOM (a desktop panel card
// and a mobile drill-down list), and the two are hand-copied: the same fact is
// spelled differently on each side, or is missing from one. The fix is not a
// shared component — the two surfaces compose their rows differently and must
// keep doing so — but a shared *derivation*: one platform-independent view
// model, and renderers that are faithful projections of it.
//
// This module holds the vocabulary of that view model plus `paintModule` /
// `paintPanel`, the traversals S1b's renderers are meant to share. Today the
// only callers are `paintPanel` and `panel.test.ts`; renderers are wired in
// S1b-3 (desktop) and S1b-4 (mobile). See `paintModule`'s docstring for what
// that does and does not buy. Everything here is data and pure functions;
// `core` may not import React or touch the DOM (`fe/core/AGENTS.md`), so a
// renderer supplies its own leaf constructors through `RowPainter<T>`.
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

/**
 * One control a row offers, **carrying its own wording**.
 *
 * The four sentences the desktop panel writes previously lived only in
 * `public.tsx`; without them here, each of S1b's two painters would re-invent
 * them — the same failure mode as `taskStatusPhrase`, which was written once
 * per surface until #1234 moved it down. This slice moves them down: they are
 * this type's fields, produced by `wave-page.ts`. `public.tsx` still spells its
 * own copies until S1b-3 rewrites the desktop panel.
 *
 * **The wording is not a function of `kind`.** `open-card` reads
 * `Open the worker card for ${task.key}` on a Task row (`public.tsx:747`) and
 * has no wording at all on a Cards row (`:517`), so the two sentences are
 * derived per row, not looked up per kind.
 *
 * **`label` and `hint` are two channels on purpose**, and must not be merged:
 * `public.tsx:743-746` states the rule — `title` describes the destination
 * *without touching the accessible name*, which stays the visible word
 * (WCAG 2.5.3). Merging them would either lose the delete target's accessible
 * name or rewrite the existing pointer text.
 */
export type RowAction = Readonly<{
  /** The accessible name for a control that has no visible text of its own;
   *  null when the control has visible text — a second `aria-label` would
   *  override that visible text (WCAG 2.5.3). */
  label: string | null;
  /** The pointer tooltip. null = offer no hint. */
  hint: string | null;
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
 * **`paintModule` now reads this table**: it filters a row's actions by it
 * before calling `row()`, so the table decides what a painter is even handed.
 * **That, and only that, is what holds today** — the guarantee is over the
 * `actions` array `row()` receives, and `panel.test.ts` is the only thing that
 * observes it.
 *
 * **Once S1b-2's `checkProjection` exists** (it does not yet — there is no such
 * function in the tree), the same filter becomes non-silent at the marker
 * level: a painter that declares an action unsupported and paints it anyway
 * will paint one `[data-nc-row-action]` too many, and one that declares it
 * supported and skips it one too few, and *that* is what will go red. Even
 * then, "painting an unsupported *control*" is not the claim, because a painter
 * may draw an extra control with no marker at all.
 *
 * What the filter cannot close is a table that contradicts itself —
 * `supported: true` next to an `undefined` callback. That is deliberately
 * left to §6.3 rather than modelled as a discriminated union: a union would at
 * most prove the callback exists in the painter's factory config, not that it
 * was bound to the element carrying the marker, and the projection framework
 * already declines to check whether a marker host is disabled or has a
 * handler. **The binding between this table and a real handler is not covered
 * by projection.**
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
 * and which will then be their single shared one. Today the only callers are
 * `paintPanel` and this module's own unit tests; the renderers are wired in
 * S1b-3 (desktop) and S1b-4 (mobile).
 *
 * **The empty state is exclusive** (#1234 §5.20): `empty()` is called when the
 * module has zero rows and *only* then, and `row()` is called for every row and
 * only when there is at least one. Leaving that to each renderer is exactly how
 * the two surfaces drifted in the first place — one of them can print an empty
 * line under a populated list, or omit the empty text entirely, and nothing
 * notices.
 *
 * **The painter's capability table is consulted here**, before `row()` is
 * called: a row is handed only the actions the painter says it supports. That
 * is what makes an unsupported action's `why` mean anything — the table now
 * decides what the painter sees, instead of being a declaration nobody reads.
 * **The guarantee stops exactly at the call boundary**: it constrains the
 * `actions` array passed to `row()`, and nothing more. Once S1b-2's
 * `checkProjection` lands, that same constraint is what will make painting one
 * `[data-nc-row-action]` too many or too few go red; until then there is no
 * marker-level carrier at all, and the only observer is `panel.test.ts`. It
 * does *not* say "an unsupported control cannot be drawn" — a painter may still
 * draw a control that carries no marker, and may put a marker on a disabled
 * element or one with no handler (§6.3, §5.26-27).
 *
 * **Three gaps stay open on purpose:**
 *
 *  - The projection check itself does not exist yet (S1b-2), so none of the
 *    marker-level claims above have a carrier in this tree today.
 *  - Nothing forces a renderer to *call* this, or `paintPanel`, at all. Both
 *    surfaces still compose their modules by hand; "both renderers are on the
 *    one branch" is a goal, not a fact this file can enforce (§6.10).
 *  - Whether a supported action is actually wired to a live handler is not
 *    checked anywhere, and will not be even after S1b-2 — see `ActionSupport`.
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

/**
 * Paint the whole panel: every row module, in the view model's order.
 *
 * **This is the desktop's traversal.** The desktop panel card lays both
 * modules out side by side in one tree, so one call produces the whole thing.
 *
 * **The mobile surface is not this.** Mobile drills down into one module at a
 * time, so on mobile the module sequence is a *navigation* structure rather
 * than a DOM sequence: each mobile page will call `paintModule` once when S1b-4
 * wires it, and there is nothing for `paintPanel` to do there. Today the mobile
 * surface calls neither.
 *
 * **Nothing forces the desktop component to call this.** The desktop can still
 * hand-compose its two modules and this function would never notice; that gap
 * is covered only by review (§6.10). Do not read this docstring as saying
 * every renderer goes through here.
 */
export function paintPanel<T>(painter: RowPainter<T>, view: WavePageView): readonly T[] {
  return view.rowModules.map((module) => paintModule(painter, module));
}

/**
 * The DOM marker attribute names — **reserved here for S1b-2/3/4**, and read by
 * nothing in production yet.
 *
 * To be precise about the present: `checkProjection` (S1b-2) does not exist,
 * the desktop painter (S1b-3) and the mobile painter (S1b-4) are not written,
 * and no `.tsx` or `.css` in the tree spells any of these names by way of this
 * table. Today the only reader is `panel.test.ts`, which pins the table as a
 * whole. Do not read this as "both surfaces already depend on these".
 *
 * They are declared now, in the one platform-independent module both surfaces
 * will depend on, because a marker name is exactly the kind of fact that
 * drifts: the stylesheet keys off one spelling, the checker off another, and a
 * painter writes a third, with every side green on its own (§3.4). Constants,
 * not types: a type would let two different strings both satisfy it.
 *
 * `core` may not import React or touch the DOM (`fe/core/AGENTS.md`), and this
 * does neither — these are platform-independent strings. **No DOM operation
 * belongs in this file.**
 *
 * `status` is spelled `data-nc-status` here, its final name. Until S1b-3
 * `public.tsx` still writes the old `data-nc-task-status`; that slice renames
 * every occurrence, and its oracle is that `rg "data-nc-task-status" fe`
 * returns zero hits. This slice defines the vocabulary and changes no existing
 * attribute.
 */
export const MARKER = Object.freeze({
  /** Bijection anchor for a row module. Carries no text obligation. */
  module: 'data-nc-module',
  /** Bijection anchor, scope and subtraction boundary for a row. Carries no
   *  text obligation of its own — every field has its own carrier. */
  row: 'data-nc-row',
  /** Value is the badge id; the element's text domain is the badge text. */
  badge: 'data-nc-badge',
  /** A host annotation, not a content marker: it does not take part in text
   *  subtraction, and `label` / `hint` are read off `aria-label` / `title`.
   *
   *  **Not `data-nc-action`**, which is already taken and is a *styling*
   *  protocol: `styles/base.css:207` gives every `[data-nc-action]` a button
   *  geometry (inline-flex, `--control-h`, border, pointer cursor) and
   *  `:225-248` freezes its value domain to `primary | secondary | tertiary |
   *  destructive` (`styles/README.md:33`). A row painter writing
   *  `data-nc-action="open-card"` would silently inherit button styling on a
   *  desktop control and on a mobile `<li>`, and hand the frozen vocabulary a
   *  fifth value. The row-scoped marker therefore gets its own name. */
  action: 'data-nc-row-action',
  /** Value is `RowStatus.token`; the element's `title` is `RowStatus.phrase`. */
  status: 'data-nc-status',
  /** A content marker whose value names which field the element carries; its
   *  text domain equals that field exactly. See `FIELD`. */
  field: 'data-nc-field',
} as const);

/** The permitted values of `MARKER.field` — one carrier per field. */
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
