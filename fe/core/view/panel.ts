// The panel view vocabulary, and the traversal panel renderers will run.
//
// #1234 — the track page's panel exists twice in the DOM (a desktop panel card
// and a mobile drill-down list), and the two are hand-copied: the same fact is
// spelled differently on each side, or is missing from one. The fix is not a
// shared component — the two surfaces compose their rows differently and must
// keep doing so — but a shared *derivation*: one platform-independent view
// model, and renderers that are faithful projections of it.
//
// This module holds the vocabulary of that view model plus `paintModule` /
// `paintPanel`, the traversals S1b's renderers are meant to share. Since S1b-3b
// the **desktop panel is one of those renderers**: `features/track/page`'s
// `desktop-painter.tsx` is a `RowPainter`, and `public.tsx` produces the panel
// card's **row modules** by calling that file's `paintDesktopPanel` wrapper —
// which calls `paintPanel` — instead of spelling their DOM inline. The panel
// card is not only those modules: the page still composes `Referenced by` and
// `Conversations` beside them, and those are router-fed slots outside the view
// model (§3.2 below). Since S1b-4a the **mobile page is a second renderer**
// (`mobile-painter.tsx`, one `paintModule` call per drill-down): Cards in
// S1b-4a, Tasks in S1b-4b, and since S1b-4b the drill-down menu is derived from
// the view model's `rowModules` rather than listed by hand. Both mobile row
// modules therefore reach this file's traversal, and neither composes a row of
// its own. See `paintModule`'s docstring for
// what that does and does not buy. Everything here is data and pure functions;
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
   *  deliberately not named in this type: it is `MARKER.status`
   *  (`data-nc-status`), declared once below. */
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
   * `aria-label` is `Status: ${phrase}` while its `title` is the bare phrase
   * (`track/page/desktop-painter.tsx`'s `statusDot`). That prefix is **renderer
   * chrome and is not in this field**, deliberately: it is a labelling decision about one
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
 * this type's fields, produced by `track-page.ts`. Since S1b-3b the desktop
 * painter reads them from here and spells none of its own. The mobile Cards row
 * (S1b-4a) writes no wording at all — both of its actions are unsupported there,
 * so it is handed none. The mobile Task row (S1b-4b) consumes all three fields
 * from here: `hint` becomes the row's pointer tooltip, `description` describes
 * its focused control, and a non-null `label` becomes its accessible name, so
 * neither surface words a control for itself.
 *
 * **The wording is not a function of `kind`.** `open-card` reads
 * `Open the worker card for ${task.key}` on a Task row and has no wording at
 * all on a Cards row's body button, so the two sentences are
 * derived per row, not looked up per kind.
 *
 * **The wording fields are separate channels on purpose**, and must not be merged:
 * `desktop-painter.tsx`'s `taskRow` states the rule — `title` describes the destination
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
  /** The finished non-visible description for the focused control. Kept
   * separate from `hint`: touch/assistive-tech users do not inherit `title`,
   * while pointer text must remain absent from the visible row body. */
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

export type TrackPageView = Readonly<{ rowModules: readonly RowModuleView[] }>;

/**
 * Whether a renderer offers an action at all. `why` is the reason it is
 * omitted, and is required so that an **explicitly declared** unsupported
 * action must state why.
 *
 * **`paintModule` reads this table**: it filters a row's actions by it before
 * calling `row()`, so the table decides what a painter is even handed. The
 * guarantee is over the `actions` array `row()` receives; `panel.test.ts`
 * observes it directly.
 *
 * **S1b-2's `checkProjection` has landed** (`tools/projection/public.ts`), and
 * makes the same filter non-silent at the marker level: a painter that declares
 * an action unsupported and paints it anyway paints one `[data-nc-row-action]`
 * too many, one that declares it supported and skips it one too few, and either
 * is `action-sequence`. Since S1b-3b the **desktop painter is run through it
 * over the real rendered page** (`desktop-projection.test.tsx`), and so is the
 * mobile one over both of its pages (`mobile-projection.test.tsx` — Cards since
 * S1b-4a, Tasks since S1b-4b), whose table is the
 * deliberate inconsistency itself. And even wired, the claim is not "an unsupported
 * *control* cannot be drawn": a painter may draw an extra control carrying no
 * marker at all.
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
 * condition for `delete-card` is `onDeleteCard !== undefined` — a *host prop*,
 * which can differ between two renders of the same platform, not a fact about
 * the platform. Since `RowPainter.action` is a fixed table, expressing that
 * means **rebuilding the painter per render** with the support bound to the
 * prop, and `makeDesktopPainter` does exactly that: a painter that hard-coded
 * `delete-card` as supported would grow a delete button on a page whose host
 * never passed a handler, which is a control that does not exist today.
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
 * and which will then be their single shared one. Its callers are `paintPanel`,
 * this module's own unit tests, and S1b-2's `checkProjection`. The **desktop**
 * production renderer reaches it through `paintPanel` as of S1b-3b
 * (`track/page/public.tsx` → `paintDesktopPanel` → `paintPanel`); **both** mobile
 * pages reach it directly through `paintMobileModule` — Cards since S1b-4a,
 * Tasks since S1b-4b — one call per drill-down.
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
 * `actions` array passed to `row()`, and nothing more. S1b-2's
 * `checkProjection` turns that same constraint into a marker-level red
 * (`action-sequence`) for painters it is run over. It does *not* say "an
 * unsupported control cannot be drawn" — a painter may still draw a control
 * that carries no marker, and may put a marker on a disabled element or one
 * with no handler (§6.3, §5.26-27).
 *
 * **Three gaps, the first of them now closed for the surfaces that exist:**
 *
 *  - The projection check runs over a **production** painter on every painted
 *    surface there is: the desktop (`desktop-projection.test.tsx`, S1b-3b) and
 *    both mobile pages (`mobile-projection.test.tsx` — Cards since S1b-4a,
 *    Tasks since S1b-4b). "Every surface" is a claim about today's two, not a
 *    rule this module can enforce on a third.
 *  - Nothing in *this file* forces a renderer to call it. What forces the
 *    desktop is a per-surface argument: `track/page/desktop-entry.test.tsx`
 *    mocks `paintDesktopPanel`, and holds both that the page calls it with the
 *    whole view and that the panel it shows is the value handed back. (The
 *    page's marker-literal source scan sits beside that as a narrower guard —
 *    it stops a literal being rewritten in place, and is not a proof the
 *    painter ran; markers reach a DOM by other routes than a literal.) That is
 *    an argument about one surface, not a rule this module can state; both
 *    mobile pages have the same pair in `mobile-entry.test.tsx` — Cards since
 *    S1b-4a, Tasks since S1b-4b (§6.10).
 *  - Whether a supported action is actually wired to a live handler is **not
 *    checked by the projection framework** — not here and not by S1b-2 (see
 *    `ActionSupport`). It is not unchecked everywhere: for the desktop's three
 *    actions `track/page/public.test.tsx` drives the real page and asserts the
 *    payload reaching the callback the action names. That cover is positive
 *    only — an extra, spurious callback on the same gesture is not excluded —
 *    and `tools/projection/public.ts`'s standing list keeps the exact terms.
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
 * than a DOM sequence: each mobile page calls `paintModule` once — Cards as of
 * S1b-4a, Tasks as of S1b-4b — and there is nothing for `paintPanel` to do
 * there. What *is* derived from `view.rowModules` on that surface is the
 * drill-down menu (S1b-4b): the module sequence drives navigation, one entry per
 * module, rather than a painted sequence of trees.
 *
 * **The desktop does reach this** (S1b-3b): `track/page/public.tsx` renders the
 * **row modules** of its panel card as `paintDesktopPanel(painter, view)`,
 * which calls this, and composes no row itself. The card around them is still
 * the page's — `Referenced by` and `Conversations` sit beside the painted
 * modules and are outside the view model.
 * What holds that is not this function — it cannot notice a caller that walks
 * away — but the page-level constraint described in `paintModule`. Do not read
 * this docstring as saying every renderer goes through here: the mobile one
 * does not.
 */
export function paintPanel<T>(painter: RowPainter<T>, view: TrackPageView): readonly T[] {
  return view.rowModules.map((module) => paintModule(painter, module));
}

/**
 * The DOM marker attribute names — read by the checker, by tests, and **since
 * S1b-3b by production renderers: the desktop panel, and since S1b-4a/4b both
 * mobile row-module pages.**
 *
 * To be precise about the present: `checkProjection` (S1b-2) reads this table in
 * full — every marker here has a selector in `tools/projection/public.ts`, and
 * `FIELD` below is its closed value domain — and
 * `features/track/page/desktop-painter.tsx` writes every one of them into the
 * desktop panel *by way of this table*, never as a literal;
 * `features/track/page/mobile-painter.tsx` does the same for both mobile pages,
 * through `ui/mobile-list`'s marker channels (which, like
 * `ui/panel-card`'s, spell the attribute names themselves — see below).
 *
 * Spellings outside this table remain, and are named so the coincidence is not
 * mistaken for a dependency:
 *
 *  - `web/src/ui/panel-card/public.tsx` writes `data-nc-module` and
 *    `data-nc-field` as **production** literals. It cannot import from here —
 *    `.dependency-cruiser.cjs`'s `ui-only-core-type-whitelist` lets `ui/**`
 *    read only `core/types/{ids,a11y}.ts` and `core/state/types.ts` — so its
 *    three marker channels take the marker *value* as a bare string and spell
 *    the attribute names themselves;
 *  - `page.module.css` selects `[data-nc-status]` literally (a stylesheet
 *    cannot import a constant), and the desktop test suites query the same
 *    literal.
 *
 * `desktop-projection.test.tsx` is what pins the desktop page's rendered markers
 * back to this table, and `mobile-projection.test.tsx` does the same for the two
 * mobile pages: their selectors are built from `MARKER`, so a literal that
 * disagreed would leave the checker finding no marker at all. That is also what
 * holds `ui/mobile-list`'s own spelling of the attribute names, which it cannot
 * import for the same dependency-cruiser reason as `ui/panel-card`.
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
 * `status` is spelled `data-nc-status`, and since S1b-3a that is the tree's
 * only spelling of the status marker. S1b-3b closed the page's half of the
 * remaining coincidence: the desktop dot's attribute is now written from this
 * table, and `public.tsx` may spell no marker name at all. `page.module.css`
 * still writes the literal, because a stylesheet has no way to read a constant;
 * what stops it drifting is that the page's rendered dot is checked against this
 * table and the stylesheet is checked against the rendered dot
 * (`task-row.browser.test.tsx`).
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
   *  protocol: `styles/base.css`'s global `[data-nc-action]` rule gives every
   *  such element a button geometry (inline-flex, `--control-h`, border,
   *  pointer cursor), and the per-value rules beside it freeze the attribute's
   *  domain to the four-way vocabulary `primary | secondary | tertiary |
   *  destructive` that `styles/README.md` documents. A row painter writing
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

/** The permitted values of `MARKER.field` — one carrier per field. **Closed, and
 *  checked as closed**: `checkProjection` reports any `data-nc-field` whose
 *  value is not one of these as `field-domain`. */
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
