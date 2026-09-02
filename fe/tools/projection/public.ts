// #1234 S1b-2 — the faithful-projection checker.
//
// `checkProjection` paints a view model with a `RowPainter`, hands the painted
// leaves to a caller-supplied `mount`, and returns every way the resulting DOM
// fails to be a faithful projection of that view model.
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
// boundaries, carried by S1b-3/4's real page tests.
//
// **Violations are returned, not thrown.** Each carries a stable `code`, so a
// malicious painter's *isolation* is mechanically assertable
// (`expect(codes).toEqual(['field-text'])`) rather than "something went red".

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

type Add = (code: ViolationCode, detail: string) => void;

function all(node: ProjectionNode, selector: string): readonly ProjectionNode[] {
  return Array.from(node.querySelectorAll(selector));
}

/**
 * The descendants matching `selector` whose **nearest `boundary` ancestor is
 * `container` itself** — i.e. the ones this container actually owns.
 *
 * A plain `querySelectorAll` would make every scoped check leak through a
 * nested container: with a module nested inside a module, the outer module's
 * `module-title` count would be 2 and a single structural fault would light up
 * four unrelated codes. Ownership scoping is what keeps each obligation's
 * malicious painter isolated to its own code.
 */
function owned(container: ProjectionNode, selector: string, boundary: string): readonly ProjectionNode[] {
  return all(container, selector)
    .filter((element) => (element.parentElement?.closest(boundary) ?? null) === container);
}

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

export function checkProjection<T>(
  painter: RowPainter<T>,
  modules: readonly RowModuleView[],
  mount: (painted: readonly T[]) => ProjectionNode,
): readonly Violation[] {
  const painted: readonly T[] = modules.map((module) => paintModule(painter, module));
  const root = mount(painted);
  const violations: Violation[] = [];
  const add: Add = (code, detail) => { violations.push({ code, detail }); };

  checkCoHosting(root, add);
  checkTree(root, modules, painter, add);
  return violations;
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

    const titles = owned(container, SELECTOR.moduleTitle, SELECTOR.module);
    if (titles.length !== 1) {
      add('field-cardinality', `${where} module-title: ${titles.length} carrier(s), expected 1`);
    } else {
      checkCarrierText(titles[0], module.title, 'field-text', `${where} module-title`, add);
    }

    // The empty state is exclusive: the text appears in a module with zero rows
    // and only there.
    const expectedEmpty = module.rows.length === 0 ? 1 : 0;
    const empties = owned(container, SELECTOR.empty, SELECTOR.module);
    if (empties.length !== expectedEmpty) {
      add('field-cardinality', `${where} empty: ${empties.length} carrier(s), expected ${expectedEmpty}`);
    } else if (expectedEmpty === 1) {
      checkCarrierText(empties[0], module.empty, 'field-text', `${where} empty`, add);
    }

    const rowElements = owned(container, SELECTOR.row, SELECTOR.module);
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

  checkPartition(root, SELECTOR.row, moduleElements, SELECTOR.module, expectedRows, 'row-partition', add);
  checkPartition(root, SELECTOR.badge, rowElements, SELECTOR.row,
    sum(rows, (row) => row.badges.length), 'badge-partition', add);
  checkPartition(root, SELECTOR.action, rowElements, SELECTOR.row,
    sum(rows, supportedCount), 'action-partition', add);
  checkPartition(root, SELECTOR.status, rowElements, SELECTOR.row,
    sum(rows, (row) => (row.status === null ? 0 : 1)), 'status-partition', add);
  checkPartition(root, SELECTOR.title, rowElements, SELECTOR.row, rows.length, 'field-partition', add);
  checkPartition(root, SELECTOR.kind, rowElements, SELECTOR.row,
    sum(rows, (row) => (row.kind === null ? 0 : 1)), 'field-partition', add);
  checkPartition(root, SELECTOR.moduleTitle, moduleElements, SELECTOR.module,
    modules.length, 'field-partition', add);
  checkPartition(root, SELECTOR.empty, moduleElements, SELECTOR.module,
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
  boundary: string,
  expectedInside: number,
  code: ViolationCode,
  add: Add,
): void {
  const inside = sum(containers, (container) => owned(container, selector, boundary).length);
  if (inside !== expectedInside) return;
  const total = all(root, selector).length;
  if (total !== inside) {
    add(code, `root holds ${total} of ${selector}, its containers own ${inside}`);
  }
}

function checkRow<T>(row: PanelRow, element: ProjectionNode, painter: RowPainter<T>, add: Add): void {
  const where = `row ${row.id}`;

  const titles = owned(element, SELECTOR.title, SELECTOR.row);
  if (titles.length !== 1) {
    add('field-cardinality', `${where} title: ${titles.length} carrier(s), expected 1`);
  } else {
    checkCarrierText(titles[0], row.title, 'field-text', `${where} title`, add);
  }

  const expectedKinds = row.kind === null ? 0 : 1;
  const kinds = owned(element, SELECTOR.kind, SELECTOR.row);
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
  const badgeElements = owned(element, SELECTOR.badge, SELECTOR.row);
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
  const found = owned(element, SELECTOR.status, SELECTOR.row);
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
 */
function checkActions<T>(row: PanelRow, element: ProjectionNode, painter: RowPainter<T>, add: Add): void {
  const supported = row.actions.filter((action) => painter.action[action.kind].supported);
  const actionElements = owned(element, SELECTOR.action, SELECTOR.row);
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
