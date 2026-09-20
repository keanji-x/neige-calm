// Same-source regression: `deriveTrackPageView` against the panel that is now rendered from it.
// No longer an independent oracle; module DOM order is excluded here and held by `checkProjectionIn`.

import { describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { card, openableCardsOf, renderPage } from './test-fixtures.tsx';

/* The fixture has to give the assertion teeth: a titled and an untitled card, a deletable card with `onDeleteCard` passed, a kernel-owned untitled card, a task with a `statusDetail`, a withdrawn task, a task with a kind and a worker card. Titles, kinds and badge texts are not substrings of one another. */
const CARDS: readonly CardWire[] = [
  card({ id: 'card-1', kind: 'shell', title: 'Main pane', deletable: true }),
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'block-1',
    key: 'alpha-gate',
    state: 'ready',
    declaration: null,
    status: 'running',
    statusDetail: 'step 2 of 3',
    kind: 'codex',
    workerCardId: 'card-1',
    pendingReason: null,
  },
  {
    blockId: 'block-2',
    key: 'beta-gate',
    state: 'withdrawn',
    declaration: 'Withdrawn',
    status: null,
    statusDetail: null,
    kind: null,
    workerCardId: null,
    pendingReason: null,
  },
];

/** Where each module's rows live in the desktop panel today; literal selectors because `no-class-dom-query` requires a static one. */
function renderedRows(container: Element, key: RowModuleView['key']): readonly Element[] {
  return key === 'cards'
    ? [...container.querySelectorAll('[data-nc-card-inventory] li')]
    : [...container.querySelectorAll('[data-nc-task-inventory] li')];
}

/** What a reader sees, and nothing a renderer says about it: `textContent` only, never `aria-label` / `title`. */
function visibleText(root: Element): string {
  return root.textContent ?? '';
}

/** Every value of `attribute` inside the row's subtree, the row element included, as a list of whole values — never joined, or membership becomes a substring test. */
function attributeValues(
  root: Element,
  attribute: 'title' | 'aria-label' | 'aria-description',
): readonly string[] {
  const carriers = attribute === 'title'
    ? root.querySelectorAll('[title]')
    : attribute === 'aria-label'
      ? root.querySelectorAll('[aria-label]')
      : root.querySelectorAll('[aria-description]');
  const values: string[] = [];
  for (const element of [root, ...carriers]) {
    const value = element.getAttribute(attribute);
    if (value !== null) values.push(value);
  }
  return values;
}

/** The row's derived action wording, against the row's own attribute values. `null` sides are not asserted. */
function expectActionWording(rowElement: Element, row: PanelRow, where: string): void {
  const titles = attributeValues(rowElement, 'title');
  const labels = attributeValues(rowElement, 'aria-label');
  const descriptions = attributeValues(rowElement, 'aria-description');
  row.actions.forEach((action, index) => {
    const at = `${where}.actions[${index}] (${action.kind})`;
    if (action.hint !== null) expect(titles, `${at}: hint`).toContain(action.hint);
    if (action.label !== null) expect(labels, `${at}: label`).toContain(action.label);
    if (action.description !== null) {
      expect(descriptions, `${at}: description`).toContain(action.description);
    }
  });
}

function occurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

/** The row's visible text fields, in no particular order. `status` is not among them: it has no text content on this surface. */
function rowFields(row: PanelRow): readonly string[] {
  return [
    row.title,
    ...(row.kind !== null ? [row.kind] : []),
    ...row.badges.map((badge) => badge.text),
  ];
}

/** The status, against the node the painter marks for it: exact equality on `title` (phrase) and `data-nc-status` (token). */
function expectStatus(rowElement: Element, row: PanelRow, where: string): void {
  const carrier = rowElement.querySelector('[data-nc-status]');
  if (row.status === null) {
    expect(carrier, `${where}: derived no status, so the page must paint no carrier`).toBeNull();
    return;
  }
  expect(carrier, `${where}: status carrier`).not.toBeNull();
  expect(carrier?.getAttribute('title'), `${where}: phrase`).toBe(row.status.phrase);
  expect(carrier?.getAttribute('data-nc-status'), `${where}: token`).toBe(row.status.token);
}

/** Every field must be present as many times as the derivation says: the untitled card's `title` and a wrongly-derived `kind` would be the same string, and one occurrence would satisfy both. */
function expectFieldsPresent(text: string, fields: readonly string[], where: string): void {
  const wanted = new Map<string, number>();
  for (const field of fields) wanted.set(field, (wanted.get(field) ?? 0) + 1);
  for (const [field, count] of wanted) {
    expect(occurrences(text, field), `${where}: ${JSON.stringify(field)} × ${count}`)
      .toBeGreaterThanOrEqual(count);
  }
}

describe('deriveTrackPageView against the rendered desktop panel', () => {
  it('renders every derived module title, and every row field inside its own row', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const view = deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) });
    const whole = visibleText(container);

    /* The fixture invariant the untitled-card arm rests on: `card-2` has no title, so its row prints its kind exactly once, and it is not deletable, so no × carries `Delete card harness`. */
    expect(CARDS[1]).toMatchObject({ title: null, deletable: false });

    for (const module of view.rowModules) {
      expect(whole).toContain(module.title);
      expect(whole).not.toContain(module.empty);

      const rendered = renderedRows(container, module.key);
      expect(rendered.length, `${module.key}: rendered rows`).toBe(module.rows.length);

      module.rows.forEach((row, index) => {
        const element = rendered[index];
        const fields = rowFields(row);
        expectFieldsPresent(visibleText(element), fields, `${module.key}[${index}]`);
        expectStatus(element, row, `${module.key}[${index}]`);
        expectActionWording(element, row, `${module.key}[${index}]`);
        for (const field of fields) expect(whole).toContain(field);
      });
    }
  });

  it('renders each module’s empty text when, and only when, the module has no rows', () => {
    const { container } = renderPage({ cards: [], tasks: [] });
    const view = deriveTrackPageView({ cards: [], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });
    const whole = visibleText(container);

    for (const module of view.rowModules) {
      expect(module.rows).toEqual([]);
      expect(whole).toContain(module.empty);
      expect(renderedRows(container, module.key).length).toBe(0);
    }
  });
});
