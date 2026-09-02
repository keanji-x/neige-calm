// #1234 S1b-2 — the faithful-projection checker.
//
// `checkProjection` paints a view model with a `RowPainter`, hands the painted
// leaves to a caller-supplied `mount`, and returns the **detected** projection
// violations of the resulting DOM against that view model.
//
// **Not "every way it can fail", deliberately.** The wording matters because the
// checker cannot carry the stronger claim: the module, badge and action layers
// return early on a sequence fault, and a carrier that is not a leaf suppresses
// the text comparison that would otherwise follow, so one fault can mask
// another within a single run. On top of that sit the trust boundaries below
// (`mount`, the production painter factory, the handler behind a marker). What
// is claimed is that each obligation named in `ViolationCode` has a carrier
// here, and that a violation of it is reported under its own code.
//
// **Why it lives in `tools/`.** It is a verification domain, and
// `.dependency-cruiser.cjs`'s `runtime-no-verification-domains` keeps `core/`
// and `web/src/` (test files excepted) from importing one. Its consumers are
// `web/src/features/**/*.test.tsx`, which that rule exempts.
//
// **Why `mount` is injected rather than done here.** `tsconfig.node.json`
// covers `tools/` with `lib: ["ES2023"]`, `types: ["node"]` and no `jsx`, so
// this file can neither render React nor name a DOM type. `ProjectionNode` is a
// structural port a real `Element` satisfies; the caller renders.
//
// **What the injection does and does not buy.** Within one call the checker
// holds the painter, so `paintModule` reads *this* painter's capability table:
// there is no second, advertised table for it to disagree with. That is the
// whole claim. It does **not** say the table cannot lie about production — the
// test and the app each call the painter factory separately — and it does
// **not** say `mount` is faithful: a `mount` that ignored `painted` and
// fabricated a correct tree would pass everything here. Both are trust
// boundaries. For the **desktop** they are carried by S1b-3b: the page is
// rendered for real and checked through `checkProjectionIn`, and
// `desktop-entry.test.tsx` holds that the page calls `paintDesktopPanel` with
// the whole view and renders what it hands back. That is the best evidence we
// have that a marked node in that tree came from the painter — not a proof:
// see the standing list below for the residue it leaves. (The page's
// marker-literal source scan is a narrower guard beside it, not the carrier;
// see `checkProjectionIn`.) Both mobile row-module pages now have the same pair
// — `mobile-projection.test.tsx` and `mobile-entry.test.tsx`, Cards since
// S1b-4a and Tasks since S1b-4b — so every surface that paints row modules is
// carried the same way. That is a statement about the two surfaces that exist,
// not a rule anything enforces on a third.
//
// **Violations are returned, not thrown.** Each carries a stable `code`, so a
// malicious painter's *isolation* is mechanically assertable
// (`expect(codes).toEqual(['field-text'])`) rather than "something went red".
//
// **Not covered by `checkProjection` itself — the standing list.** A green
// `checkProjection` says nothing about any of these. Some have no carrier
// anywhere; others have a **partial** desktop carrier outside this framework,
// and each entry says which:
//
//  - **`mount` fidelity** and **the production painter factory** — the two trust
//    boundaries named above; S1b-3/4's real page tests carry them.
//  - **Handler binding — the positive binding *and* the panel's own rival
//    callback have a desktop carrier; the wider prop surface is not
//    exhausted.** This framework checks none of it:
//    whether a marked host runs anything, whether the payload (`cardId` /
//    `blockId`) reaches the right callback, whether that callback is the one the
//    row's action names. A painter that wires every action to the same handler
//    is green here. Outside it, `wave/page/public.test.tsx` drives the real
//    desktop page and asserts, for each of the three actions that exist today,
//    that the callback it must reach was called with the right payload —
//    `onOpenTask` with the block id, `onOpenCard` with the worker card's id,
//    `onDeleteCard` with the row's wire id exactly once — plus that no delete is
//    offered without `onDeleteCard` or on a kernel-owned card. **Exclusivity is
//    covered for the three actions that exist, and for nothing else.** Each of
//    them pins a second callback as *not* called: the two worker-card cases pin
//    `onOpenCard` / `onOpenTask` against each other, and the delete case
//    supplies `onOpenCard` and asserts it was never reached — so a delete that
//    fires the correct callback **and** opens the card goes red. The **mobile**
//    surface has its own, narrower half: `reveal-block` is the one action it
//    supports, and `wave/page/public.test.tsx` taps a real mobile Task row and
//    asserts `onOpenTask` was called once with the block id. That case pins no
//    rival callback, so mobile has positive binding and payload but no
//    exclusivity. What remains unexhausted on both surfaces is the rest of the
//    prop surface: no case enumerates *every* callback the page takes, so an
//    action that additionally fires some third prop (`onRenameWave`, say) is
//    still green. A fourth action arrives with no cover at all, and so would a
//    third surface.
//  - **Interactivity of the host.** §6.3 declines this: a marker may sit on a
//    `disabled` control, on an element with no role or tab stop, or on a plain
//    `<span>`. Mobile rows require it — Astryx generates the interactive element
//    the painter cannot reach.
//  - **Module-head slots.** The `+` menu, conversations and backlinks (§3.2,
//    D2) are router-composed `ReactNode`s outside the view model; they carry
//    zero mechanical obligation.
//  - **Alternate accessible naming.** `label` is read off `aria-label` only, so
//    a host named by `aria-labelledby`, a `<label>`, or its own visible text
//    goes **falsely red** against a non-null `label`. The painters must use
//    `aria-label`; that constraint is not itself checked, it is imposed by this
//    checker being the oracle.
//  - **`RowBadge.struck` — a formal field with no carrier in this framework at
//    all.** `checkBadges` reads a badge's id, its position in the sequence and
//    its text, and nothing else: a painter that ignored `struck`, or inverted
//    it, is green under every code above. Both surfaces cover it *outside* the
//    projection, and neither of those is a projection obligation. Desktop:
//    `wave/page/public.test.tsx` asserts the `taskWithdrawn` class on a
//    withdrawn declaration badge and its absence on an ordinary one. Mobile:
//    `wave/page/mobile-painter.test.tsx` makes the same both-ways class
//    assertion, and `wave/page/mobile-task-row.browser.test.tsx` goes one
//    further — it reads `text-decoration-line` back out of the cascade in a real
//    engine, which is the half a class assertion cannot reach on either side.
//  - **The projection is not onto, by design.** Nothing here says the DOM holds
//    *only* what the view model names. A painter may add unmarked chrome,
//    wrappers, counters and extra controls freely and stay green — that is a
//    standing positive case in `projection-contract.test.tsx` ("when the painter
//    invents chrome of its own"), not an accident. Every code above counts
//    marked nodes, so an extra affordance carrying no marker is invisible to
//    all of them; whether the surface shows something it should not is a
//    question for the behaviour suites.
//  - **The semantic content of the view model.** That a badge's `text` or a
//    status's `phrase` is the *right* string is `core/view`'s business. This
//    checker only proves the DOM says what the view model says.
//  - **Completeness of the shape guard.** The fixture-shape list in
//    `projection-contract.test.tsx` is mechanically *executed*, but its
//    *contents* — including any field a future `RowBadge` / `RowStatus` grows —
//    are maintained by hand (§6.9).
//  - **What the page's source scan does not close.** `public.tsx` may spell no
//    `MARKER` name, and that stops a marker literal being rewritten in place —
//    it does not stop one reaching the DOM by a computed property
//    (`{...{[MARKER.module]: 'cards'}}`), by assembly (`'data-' +
//    'nc-module'`), through a marker-channel prop (`ui/panel-card` takes
//    three), or from an imported component that carries its own. The scan is
//    a hygiene guard; `desktop-entry.test.tsx` is the oracle.
//  - **What the entry oracle does not close either — content the painter did
//    not produce.** `desktop-entry.test.tsx` catches the painter's whole return
//    value being discarded, and catches a bypass that carries markers. Its
//    `replace` case additionally asserts that the fixture's own module titles
//    and row text vanish with the painter's output, which catches a *hand-built,
//    unmarked* second copy of the same **Cards/Tasks row-modules** rendered
//    beside the real one. But that half is bound to **that fixture's strings**:
//    a parallel unmarked tree printing different content, or one that only
//    appears for inputs the fixture does not use, is outside every check we
//    have. The unproven statement is therefore "**all Cards/Tasks row-module
//    content comes from the painter**" — deliberately not "all content in the
//    desktop panel", which is false as an obligation: the panel legitimately
//    holds `Referenced by` and `Conversations`, composed by the page outside the
//    painter.
//  - **The characterization suite is no longer independent.**
//    `wave/page/view-characterization.test.tsx` compared the derivation against
//    a hand-written page; since S1b-3b the page renders *from* that derivation,
//    so a misread rule now makes both sides wrong together and the suite stays
//    green. It is a same-source regression test now, and its own head says so.
//  - **No mechanical old-vs-new DOM diff.** That the panel S1b-3b paints is
//    structurally identical to the one the page hand-composed before it is held
//    by the surviving behaviour, contract and browser suites plus review — not
//    by a whole-subtree structural comparison against the pre-slice tree.
//    Anything those suites do not read (a class name nobody asserts, an
//    attribute order, a wrapper element) could have changed unseen.

import { FIELD, MARKER, paintModule } from '../../core/view/panel.ts';
import type { PanelRow, RowModuleView, RowPainter } from '../../core/view/panel.ts';

/**
 * The structural shape the checker needs from a rendered node. A real DOM
 * `Element` satisfies it, so a caller passes `render(...).container` straight
 * in.
 *
 * **Do not add `contains`.** `Element.contains` takes a `Node` — a
 * contravariant position — and `ProjectionNode` is not a `Node`, so adding it
 * makes a real `Element` unassignable to this type (TS2322). Non-nesting is
 * checked with `parentElement?.closest(...)` instead, whose position is
 * covariant.
 *
 * **Do not add `childNodes`.** Ordered text-node traversal would only be needed
 * for the text-domain subtraction this slice deliberately does not do; see
 * `checkCarrierText`.
 */
export type ProjectionNode = Readonly<{
  getAttribute: (name: string) => string | null;
  textContent: string | null;
  parentElement: ProjectionNode | null;
  querySelectorAll: (selectors: string) => ArrayLike<ProjectionNode> & Iterable<ProjectionNode>;
  closest: (selectors: string) => ProjectionNode | null;
  /** Needed because `querySelectorAll` excludes the element itself while
   *  ownership (see `owned`) includes it. Its parameter is a `string`, so unlike
   *  `contains` it costs no assignability. */
  matches: (selectors: string) => boolean;
}>;

/** One name per obligation. See `checkProjection`'s note on isolation. */
export type ViolationCode =
  | 'module-sequence'
  | 'module-nesting'
  | 'row-sequence'
  | 'row-nesting'
  | 'row-partition'
  | 'badge-sequence'
  | 'badge-nesting'
  | 'badge-partition'
  | 'badge-text'
  | 'action-sequence'
  | 'action-nesting'
  | 'action-partition'
  | 'action-label'
  | 'action-hint'
  | 'status-cardinality'
  | 'status-partition'
  | 'status-token'
  | 'status-phrase'
  | 'field-cardinality'
  | 'field-domain'
  | 'field-partition'
  | 'field-text'
  | 'carrier-not-leaf'
  | 'marker-co-host';

export type Violation = Readonly<{ code: ViolationCode; detail: string }>;

/** Attribute selectors, spelled from `MARKER` / `FIELD` so no name is retyped. */
const SELECTOR = Object.freeze({
  module: `[${MARKER.module}]`,
  row: `[${MARKER.row}]`,
  badge: `[${MARKER.badge}]`,
  action: `[${MARKER.action}]`,
  status: `[${MARKER.status}]`,
  anyField: `[${MARKER.field}]`,
  title: `[${MARKER.field}="${FIELD.title}"]`,
  kind: `[${MARKER.field}="${FIELD.kind}"]`,
  moduleTitle: `[${MARKER.field}="${FIELD.moduleTitle}"]`,
  empty: `[${MARKER.field}="${FIELD.empty}"]`,
} as const);

/**
 * The content markers — the five that carry meaning from the view model.
 * `MARKER.action` is a *host annotation*: it owns no field text and may share
 * an element with a control that does, so it is deliberately absent here.
 */
const CONTENT_MARKERS: readonly string[] = Object.freeze([
  MARKER.module, MARKER.row, MARKER.badge, MARKER.status, MARKER.field,
]);

const ANY_CONTENT_MARKER = CONTENT_MARKERS.map((name) => `[${name}]`).join(',');

/** The closed value domain of `MARKER.field`, read off `FIELD` so no name is
 *  retyped. See `checkFieldDomain`. */
const FIELD_VALUES: readonly string[] = Object.freeze(Object.values(FIELD));

type Add = (code: ViolationCode, detail: string) => void;

function all(node: ProjectionNode, selector: string): readonly ProjectionNode[] {
  return Array.from(node.querySelectorAll(selector));
}

/**
 * The elements matching `selector` that `container` **owns**: the container
 * itself when it matches, plus every descendant whose **nearest `boundary`
 * ancestor is `container`**, in document order.
 *
 * A plain `querySelectorAll` would make every scoped check leak through a
 * nested container: with a module nested inside a module, the outer module's
 * `module-title` count would be 2 and a single structural fault would light up
 * four unrelated codes. Ownership scoping is what keeps each obligation's
 * malicious painter isolated to its own code.
 *
 * **The container itself is in scope, and must be.** An earlier draft started
 * the search at `element.parentElement`, which excludes the container, and two
 * legitimate shapes went falsely red as a result:
 *
 *  - **Mobile rows are their own action host** (§3.5): the whole `<li>` is
 *    tappable, so `<li data-nc-row data-nc-row-action="open-card">` is the
 *    correct shape — and a strict-descendant scope reads it as a row with *zero*
 *    actions, i.e. `action-sequence` against a faithful painter.
 *  - **A marker on a nested container's own root** was attributed to the
 *    *enclosing* container rather than to itself.
 *
 * It also decides where a co-hosted content marker is counted: a status painted
 * on the row root is that row's one status (so `status-cardinality` is quiet and
 * `marker-co-host` — the obligation actually broken — is the only code). Note
 * `element.closest(boundary)` matches `element` itself, which is exactly what
 * makes the descendant clause agree with the self clause. Non-nesting is a
 * different question and uses `hasAncestor`, which must exclude self.
 */
function owned(container: ProjectionNode, selector: string, boundary: string): readonly ProjectionNode[] {
  const inside = all(container, selector).filter((element) => element.closest(boundary) === container);
  return container.matches(selector) ? [container, ...inside] : inside;
}

/** A container's owned set for one selector — the one scope helper the scoped
 *  checks and `checkPartition` share, so neither can drift from the other. */
type Scope = (container: ProjectionNode) => readonly ProjectionNode[];

const inModule = (selector: string): Scope =>
  (container) => owned(container, selector, SELECTOR.module);
const inRow = (selector: string): Scope =>
  (container) => owned(container, selector, SELECTOR.row);

/**
 * The action layer's scope. It is `inRow(SELECTOR.action)` and is named because
 * two callers must share it: `checkActions` and the `action-partition` sum. The
 * row element itself is included when it carries the action marker — the mobile
 * shape, where the whole `<li>` is the tappable control (§3.5).
 */
const actionScope: Scope = inRow(SELECTOR.action);

/** `el.closest` matches `el` itself, so the ancestor search starts at the parent. */
function hasAncestor(node: ProjectionNode, selector: string): boolean {
  return (node.parentElement?.closest(selector) ?? null) !== null;
}

function sameSequence(actual: readonly (string | null)[], expected: readonly string[]): boolean {
  return actual.length === expected.length && actual.every((value, index) => value === expected[index]);
}

function show(values: readonly (string | null)[]): string {
  return JSON.stringify(values);
}

/**
 * Run every obligation against a tree that **already exists** — the entry a real
 * page's test uses (#1234 S1b-3b): render the production component, take the
 * surface's own subtree, and hand it here.
 *
 * **Honest accounting: this is one step weaker than `checkProjection`.** The
 * `painter` argument is the *caller's declaration*. Nothing here can prove that
 * `root` was painted by this painter — or by any painter at all. Two facts are
 * read off `painter` and both are taken on trust:
 *
 *  - its capability table, which decides the expected action sequence
 *    (`checkActions`, `action-partition`);
 *  - by implication, that the production surface constructed its painter with
 *    the same host props the test did.
 *
 * A page that hand-composed a correct-looking tree, or that painted with a
 * *different* painter, is green here. `checkProjection` at least paints with the
 * painter it checks; this entry does not paint at all.
 *
 * **What closes that gap is not in this file**, and on the desktop it is two
 * things, not one:
 *
 *  - `features/wave/page/desktop-entry.test.tsx` mocks `paintDesktopPanel` and
 *    holds the *call*: the page invokes it once, with the whole derived view,
 *    and the panel it shows is the value that came back — a marked node in the
 *    real DOM therefore came from the painter. This is the load-bearing half,
 *    and it mentions no marker name, so no spelling can go round it.
 *  - The page's source scan (`desktop-projection.test.tsx`) is the narrower
 *    guard beside it: `public.tsx` may not spell any of `MARKER`'s attribute
 *    names, in either the kebab (`data-nc-row`) or the camel (`dataset.ncRow`)
 *    spelling. Read it as "the page does not rewrite a marker literal in
 *    place", **not** as a closed proof — a computed property, a concatenation,
 *    a marker-channel prop or an imported component all reach the DOM without
 *    one.
 *
 * A green result here means nothing without the first of those.
 */
export function checkProjectionIn<T>(
  painter: RowPainter<T>,
  modules: readonly RowModuleView[],
  root: ProjectionNode,
): readonly Violation[] {
  const violations: Violation[] = [];
  const add: Add = (code, detail) => { violations.push({ code, detail }); };

  checkFieldDomain(root, add);
  checkCoHosting(root, add);
  checkTree(root, modules, painter, add);
  return violations;
}

/**
 * Paint with `painter`, hand the leaves to the injected `mount`, and check the
 * resulting tree. Behaviour is unchanged by the `checkProjectionIn` split: this
 * is the same three checks over the same root, and `projection-contract.test.tsx`
 * is the safety net that says so.
 */
export function checkProjection<T>(
  painter: RowPainter<T>,
  modules: readonly RowModuleView[],
  mount: (painted: readonly T[]) => ProjectionNode,
): readonly Violation[] {
  const painted: readonly T[] = modules.map((module) => paintModule(painter, module));
  return checkProjectionIn(painter, modules, mount(painted));
}

/**
 * **C — `data-nc-field` has a closed value domain.** Every carrier's value must
 * be one of `FIELD`'s.
 *
 * Without this the only observers of the attribute are the four value-specific
 * selectors in `SELECTOR`, and a misspelled or invented field name matches none
 * of them: `data-nc-field="titel"` makes the row's real title carrier vanish
 * from the cardinality count *and* leaves the typo entirely unreported, so the
 * red points at the wrong obligation. `FIELD` calls its members the permitted
 * values; a closed set is checked as a closed set.
 */
function checkFieldDomain(root: ProjectionNode, add: Add): void {
  for (const element of all(root, SELECTOR.anyField)) {
    const value = element.getAttribute(MARKER.field);
    if (value === null || !FIELD_VALUES.includes(value)) {
      add('field-domain', `${MARKER.field}=${show([value])} is not one of ${show(FIELD_VALUES)}`);
    }
  }
}

/**
 * **E — at most one content marker per element.** Without it a field carrier
 * and a row carrier can be the same element, and "a field may only be satisfied
 * by its own carrier" stops meaning anything.
 */
function checkCoHosting(root: ProjectionNode, add: Add): void {
  for (const element of all(root, ANY_CONTENT_MARKER)) {
    const carried = CONTENT_MARKERS.filter((name) => element.getAttribute(name) !== null);
    if (carried.length > 1) add('marker-co-host', `one element carries ${carried.join(' + ')}`);
  }
}

/**
 * A carrier that owes an exact string may owe only its own: it must hold no
 * descendant content marker.
 *
 * This replaces v7.2's text-domain subtraction, which this port cannot
 * implement — without `childNodes` the checker cannot see where a text node
 * sits relative to an element child, so `<c><k>a</k>bac</c>` and
 * `<c>ab<k>a</k>c</c>` are indistinguishable to it while their text domains
 * differ. Once every field has a leaf carrier of its own, subtraction has
 * nothing left to do.
 */
function checkCarrierText(
  carrier: ProjectionNode,
  expected: string,
  code: 'field-text' | 'badge-text',
  where: string,
  add: Add,
): void {
  const nested = all(carrier, ANY_CONTENT_MARKER);
  if (nested.length > 0) {
    add('carrier-not-leaf', `${where} holds ${nested.length} descendant content marker(s)`);
    return;
  }
  if (carrier.textContent !== expected) {
    add(code, `${where}: ${show([carrier.textContent])} != ${show([expected])}`);
  }
}

function checkTree<T>(
  root: ProjectionNode,
  modules: readonly RowModuleView[],
  painter: RowPainter<T>,
  add: Add,
): void {
  const moduleElements = all(root, SELECTOR.module);
  const keys = moduleElements.map((element) => element.getAttribute(MARKER.module));
  if (!sameSequence(keys, modules.map((module) => module.key))) {
    // Everything below pairs containers to view-model entries by index, which is
    // meaningless once the sequence is wrong. `CSS.escape` does not exist in
    // jsdom (`window.CSS === undefined`), so lookup by id is not an option.
    add('module-sequence', `root modules ${show(keys)} != ${show(modules.map((m) => m.key))}`);
    return;
  }
  for (const element of moduleElements) {
    if (hasAncestor(element, SELECTOR.module)) {
      add('module-nesting', `module ${show([element.getAttribute(MARKER.module)])} sits inside another module`);
    }
  }
  // **There is no `module-partition` obligation, deliberately.** A layer's
  // partition check asks "does the root hold more of this marker than its
  // enclosing containers do?" — and the module layer's enclosing container *is*
  // the root, so the question is an identity. A nested module is
  // `module-nesting`; an extra root-level one is `module-sequence`. Recorded so
  // the missing fourth code does not read as an oversight.

  const rowCarriers: { row: PanelRow; element: ProjectionNode }[] = [];
  let expectedRows = 0;

  modules.forEach((module, index) => {
    const container = moduleElements[index];
    const where = `module ${module.key}`;
    expectedRows += module.rows.length;

    const titles = inModule(SELECTOR.moduleTitle)(container);
    if (titles.length !== 1) {
      add('field-cardinality', `${where} module-title: ${titles.length} carrier(s), expected 1`);
    } else {
      checkCarrierText(titles[0], module.title, 'field-text', `${where} module-title`, add);
    }

    // The empty state is exclusive: the text appears in a module with zero rows
    // and only there.
    const expectedEmpty = module.rows.length === 0 ? 1 : 0;
    const empties = inModule(SELECTOR.empty)(container);
    if (empties.length !== expectedEmpty) {
      add('field-cardinality', `${where} empty: ${empties.length} carrier(s), expected ${expectedEmpty}`);
    } else if (expectedEmpty === 1) {
      checkCarrierText(empties[0], module.empty, 'field-text', `${where} empty`, add);
    }

    const rowElements = inModule(SELECTOR.row)(container);
    const ids = rowElements.map((element) => element.getAttribute(MARKER.row));
    if (!sameSequence(ids, module.rows.map((row) => row.id))) {
      add('row-sequence', `${where} rows ${show(ids)} != ${show(module.rows.map((r) => r.id))}`);
      return;
    }
    for (const element of rowElements) {
      if (hasAncestor(element, SELECTOR.row)) {
        add('row-nesting', `${where} row ${show([element.getAttribute(MARKER.row)])} sits inside another row`);
      }
    }
    module.rows.forEach((row, rowIndex) => {
      rowCarriers.push({ row, element: rowElements[rowIndex] });
    });
  });

  for (const { row, element } of rowCarriers) checkRow(row, element, painter, add);

  // Partition completeness only means anything once every container has been
  // paired; a module whose row sequence was already reported has no usable
  // container list, and re-deriving one would report the same fault twice.
  if (rowCarriers.length !== expectedRows) return;

  const rowElements = rowCarriers.map((entry) => entry.element);
  const rows = rowCarriers.map((entry) => entry.row);
  const supportedCount = (row: PanelRow): number =>
    row.actions.filter((action) => painter.action[action.kind].supported).length;

  checkPartition(root, SELECTOR.row, moduleElements, inModule(SELECTOR.row), expectedRows, 'row-partition', add);
  checkPartition(root, SELECTOR.badge, rowElements, inRow(SELECTOR.badge),
    sum(rows, (row) => row.badges.length), 'badge-partition', add);
  // The action layer reuses `actionScope`, the very helper `checkActions` reads,
  // so a row that hosts its own action marker cannot be owned by one and
  // disowned by the other.
  checkPartition(root, SELECTOR.action, rowElements, actionScope,
    sum(rows, supportedCount), 'action-partition', add);
  checkPartition(root, SELECTOR.status, rowElements, inRow(SELECTOR.status),
    sum(rows, (row) => (row.status === null ? 0 : 1)), 'status-partition', add);
  checkPartition(root, SELECTOR.title, rowElements, inRow(SELECTOR.title), rows.length, 'field-partition', add);
  checkPartition(root, SELECTOR.kind, rowElements, inRow(SELECTOR.kind),
    sum(rows, (row) => (row.kind === null ? 0 : 1)), 'field-partition', add);
  checkPartition(root, SELECTOR.moduleTitle, moduleElements, inModule(SELECTOR.moduleTitle),
    modules.length, 'field-partition', add);
  checkPartition(root, SELECTOR.empty, moduleElements, inModule(SELECTOR.empty),
    sum(modules, (module) => (module.rows.length === 0 ? 1 : 0)), 'field-partition', add);
}

function sum<E>(items: readonly E[], of: (item: E) => number): number {
  return items.reduce((total, item) => total + of(item), 0);
}

/**
 * **Partition completeness.** The root's total for a marker must equal the sum
 * over the containers that own it. This is what stops a duplicate painted
 * *outside* its enclosing container but inside the root — a shape that the
 * sequence, nesting and cardinality checks all pass over, and which jsdom
 * measurement showed staying green through all three.
 *
 * When the owned sum already disagrees with the view model, a scoped check has
 * reported the same fault and this one stays quiet.
 */
function checkPartition(
  root: ProjectionNode,
  selector: string,
  containers: readonly ProjectionNode[],
  scope: Scope,
  expectedInside: number,
  code: ViolationCode,
  add: Add,
): void {
  const inside = sum(containers, (container) => scope(container).length);
  if (inside !== expectedInside) return;
  const total = all(root, selector).length;
  if (total !== inside) {
    add(code, `root holds ${total} of ${selector}, its containers own ${inside}`);
  }
}

function checkRow<T>(row: PanelRow, element: ProjectionNode, painter: RowPainter<T>, add: Add): void {
  const where = `row ${row.id}`;

  const titles = inRow(SELECTOR.title)(element);
  if (titles.length !== 1) {
    add('field-cardinality', `${where} title: ${titles.length} carrier(s), expected 1`);
  } else {
    checkCarrierText(titles[0], row.title, 'field-text', `${where} title`, add);
  }

  const expectedKinds = row.kind === null ? 0 : 1;
  const kinds = inRow(SELECTOR.kind)(element);
  if (kinds.length !== expectedKinds) {
    add('field-cardinality', `${where} kind: ${kinds.length} carrier(s), expected ${expectedKinds}`);
  } else if (row.kind !== null) {
    checkCarrierText(kinds[0], row.kind, 'field-text', `${where} kind`, add);
  }

  checkBadges(row, element, add);
  checkStatus(row, element, add);
  checkActions(row, element, painter, add);
}

function checkBadges(row: PanelRow, element: ProjectionNode, add: Add): void {
  const badgeElements = inRow(SELECTOR.badge)(element);
  const ids = badgeElements.map((badge) => badge.getAttribute(MARKER.badge));
  if (!sameSequence(ids, row.badges.map((badge) => badge.id))) {
    add('badge-sequence', `row ${row.id} badges ${show(ids)} != ${show(row.badges.map((b) => b.id))}`);
    return;
  }
  for (const badge of badgeElements) {
    if (hasAncestor(badge, SELECTOR.badge)) {
      add('badge-nesting', `row ${row.id} badge ${show([badge.getAttribute(MARKER.badge)])} sits inside another badge`);
    }
  }
  row.badges.forEach((badge, index) => {
    checkCarrierText(badgeElements[index], badge.text, 'badge-text', `row ${row.id} badge ${badge.id}`, add);
  });
}

/**
 * **B — status is exact on both sides.** `status === null` means *zero*
 * `[data-nc-status]` in the row, not "at most one": v7's draft left the null
 * side unstated, and a status painted on a status-less row stayed green.
 */
function checkStatus(row: PanelRow, element: ProjectionNode, add: Add): void {
  const found = inRow(SELECTOR.status)(element);
  const expected = row.status === null ? 0 : 1;
  if (found.length !== expected) {
    add('status-cardinality', `row ${row.id}: ${found.length} status element(s), expected ${expected}`);
    return;
  }
  if (row.status === null) return;
  const [status] = found;
  const token = status.getAttribute(MARKER.status);
  if (token !== row.status.token) {
    add('status-token', `row ${row.id} token ${show([token])} != ${show([row.status.token])}`);
  }
  const phrase = status.getAttribute('title');
  if (phrase !== row.status.phrase) {
    add('status-phrase', `row ${row.id} phrase ${show([phrase])} != ${show([row.status.phrase])}`);
  }
}

/**
 * **A (action layer) + D.** The expected sequence is over *supported* kinds
 * only, so an unsupported action painted anyway is one marker too many and a
 * supported one skipped is one too few. That is the claim, and it stops there:
 * a painter may still draw an extra control carrying no marker at all, and may
 * put a marker on a disabled element or one with no handler (§6.3).
 *
 * `label` and `hint` are asserted on **both** sides. A one-sided check misses
 * the real regression: an `aria-label` handed to a control that has visible
 * text overrides that text (WCAG 2.5.3).
 *
 * **The row element is in scope as its own action host** — see `actionScope`.
 */
function checkActions<T>(row: PanelRow, element: ProjectionNode, painter: RowPainter<T>, add: Add): void {
  const supported = row.actions.filter((action) => painter.action[action.kind].supported);
  const actionElements = actionScope(element);
  const kinds = actionElements.map((action) => action.getAttribute(MARKER.action));
  if (!sameSequence(kinds, supported.map((action) => action.kind))) {
    add('action-sequence', `row ${row.id} actions ${show(kinds)} != ${show(supported.map((a) => a.kind))}`);
    return;
  }
  for (const action of actionElements) {
    if (hasAncestor(action, SELECTOR.action)) {
      add('action-nesting', `row ${row.id} action ${show([action.getAttribute(MARKER.action)])} sits inside another action`);
    }
  }
  supported.forEach((action, index) => {
    const host = actionElements[index];
    const label = host.getAttribute('aria-label');
    if (label !== action.label) {
      add('action-label', `row ${row.id} action ${action.kind} aria-label ${show([label])} != ${show([action.label])}`);
    }
    const hint = host.getAttribute('title');
    if (hint !== action.hint) {
      add('action-hint', `row ${row.id} action ${action.kind} title ${show([hint])} != ${show([action.hint])}`);
    }
  });
}
