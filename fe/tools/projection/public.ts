// The faithful-projection checker: paints a view model with a `RowPainter`, hands the leaves to a
// caller-supplied `mount`, and returns the DETECTED projection violations of the resulting DOM.
// Layers return early on a sequence fault, so one fault can mask another. `mount` fidelity, the
// production painter factory and handler binding are trust boundaries outside this file; `label` is
// read off `aria-label` only; the projection is not onto (unmarked chrome is invisible to every code).

import { FIELD, MARKER, paintModule } from '../../core/view/panel.ts';
import type { PanelRow, RowModuleView, RowPainter } from '../../core/view/panel.ts';

/**
 * The structural shape the checker needs from a rendered node; a real `Element` satisfies it.
 * Do not add `contains`: its `Node` parameter is contravariant and would make `Element` unassignable (TS2322).
 */
export type ProjectionNode = Readonly<{
  getAttribute: (name: string) => string | null;
  textContent: string | null;
  parentElement: ProjectionNode | null;
  querySelectorAll: (selectors: string) => ArrayLike<ProjectionNode> & Iterable<ProjectionNode>;
  closest: (selectors: string) => ProjectionNode | null;
  /** `querySelectorAll` excludes the element itself while ownership includes it. */
  matches: (selectors: string) => boolean;
}>;

/** One name per obligation. */
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

/** The content markers. `MARKER.action` is a host annotation that may share an element with a control, so it is absent here. */
const CONTENT_MARKERS: readonly string[] = Object.freeze([
  MARKER.module, MARKER.row, MARKER.badge, MARKER.status, MARKER.field,
]);

const ANY_CONTENT_MARKER = CONTENT_MARKERS.map((name) => `[${name}]`).join(',');

/** The closed value domain of `MARKER.field`. */
const FIELD_VALUES: readonly string[] = Object.freeze(Object.values(FIELD));

type Add = (code: ViolationCode, detail: string) => void;

function all(node: ProjectionNode, selector: string): readonly ProjectionNode[] {
  return Array.from(node.querySelectorAll(selector));
}

/**
 * The elements matching `selector` that `container` owns: itself when it matches, plus every
 * descendant whose nearest `boundary` ancestor is `container`. The container itself must be in scope:
 * a mobile row is its own action host (`<li data-nc-row data-nc-row-action>`).
 */
function owned(container: ProjectionNode, selector: string, boundary: string): readonly ProjectionNode[] {
  const inside = all(container, selector).filter((element) => element.closest(boundary) === container);
  return container.matches(selector) ? [container, ...inside] : inside;
}

/** A container's owned set for one selector, shared by the scoped checks and `checkPartition`. */
type Scope = (container: ProjectionNode) => readonly ProjectionNode[];

const inModule = (selector: string): Scope =>
  (container) => owned(container, selector, SELECTOR.module);
const inRow = (selector: string): Scope =>
  (container) => owned(container, selector, SELECTOR.row);

/** The action layer's scope, shared by `checkActions` and the `action-partition` sum. */
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
 * Run every obligation against a tree that already exists. One step weaker than `checkProjection`:
 * `painter` is the caller's declaration and nothing here proves `root` was painted by it — that is
 * held by the page's entry test, not by this file.
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

/** Paint with `painter`, hand the leaves to the injected `mount`, and check the resulting tree. */
export function checkProjection<T>(
  painter: RowPainter<T>,
  modules: readonly RowModuleView[],
  mount: (painted: readonly T[]) => ProjectionNode,
): readonly Violation[] {
  const painted: readonly T[] = modules.map((module) => paintModule(painter, module));
  return checkProjectionIn(painter, modules, mount(painted));
}

/**
 * `data-nc-field` has a closed value domain. Without this a misspelled field name matches no
 * value-specific selector and the red points at the wrong obligation.
 */
function checkFieldDomain(root: ProjectionNode, add: Add): void {
  for (const element of all(root, SELECTOR.anyField)) {
    const value = element.getAttribute(MARKER.field);
    if (value === null || !FIELD_VALUES.includes(value)) {
      add('field-domain', `${MARKER.field}=${show([value])} is not one of ${show(FIELD_VALUES)}`);
    }
  }
}

/** At most one content marker per element, or "a field may only be satisfied by its own carrier" stops meaning anything. */
function checkCoHosting(root: ProjectionNode, add: Add): void {
  for (const element of all(root, ANY_CONTENT_MARKER)) {
    const carried = CONTENT_MARKERS.filter((name) => element.getAttribute(name) !== null);
    if (carried.length > 1) add('marker-co-host', `one element carries ${carried.join(' + ')}`);
  }
}

/**
 * A carrier that owes an exact string must hold no descendant content marker: without `childNodes`
 * the checker cannot tell `<c><k>a</k>bac</c>` from `<c>ab<k>a</k>c</c>`.
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
    // Everything below pairs containers to view-model entries by index, which is meaningless once the
    // sequence is wrong. `CSS.escape` does not exist in jsdom, so lookup by id is not an option.
    add('module-sequence', `root modules ${show(keys)} != ${show(modules.map((m) => m.key))}`);
    return;
  }
  for (const element of moduleElements) {
    if (hasAncestor(element, SELECTOR.module)) {
      add('module-nesting', `module ${show([element.getAttribute(MARKER.module)])} sits inside another module`);
    }
  }
  // No `module-partition` obligation: the module layer's enclosing container IS the root, so the
  // question is an identity. A nested module is `module-nesting`; an extra root-level one is `module-sequence`.

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

  // Partition completeness only means anything once every container has been paired.
  if (rowCarriers.length !== expectedRows) return;

  const rowElements = rowCarriers.map((entry) => entry.element);
  const rows = rowCarriers.map((entry) => entry.row);
  const supportedCount = (row: PanelRow): number =>
    row.actions.filter((action) => painter.action[action.kind].supported).length;

  checkPartition(root, SELECTOR.row, moduleElements, inModule(SELECTOR.row), expectedRows, 'row-partition', add);
  checkPartition(root, SELECTOR.badge, rowElements, inRow(SELECTOR.badge),
    sum(rows, (row) => row.badges.length), 'badge-partition', add);
  // Reuses `actionScope`, so a row hosting its own action marker cannot be owned by one check and disowned by the other.
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
 * Partition completeness: the root's total for a marker must equal the sum over the containers that
 * own it, which catches a duplicate painted outside its container but inside the root. Quiet when a
 * scoped check has already reported the fault.
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

/** Status is exact on both sides: `status === null` means ZERO `[data-nc-status]` in the row, not "at most one". */
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
 * The expected sequence is over supported kinds only. `label` and `hint` are asserted on BOTH sides:
 * an `aria-label` handed to a control with visible text overrides that text (WCAG 2.5.3).
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
