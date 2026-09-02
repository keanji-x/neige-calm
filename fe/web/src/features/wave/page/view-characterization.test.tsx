// Characterization: `deriveWavePageView` against the panel as it stands.
//
// #1234 S1a derives the wave panel's view model in `core/view` but wires
// nothing — `public.tsx` is untouched by this slice, deliberately, because this
// suite uses it as the oracle. If the derivation misread the desktop panel (the
// `kind` condition, the ownership badge, the `statusDetail` join), S1a would be
// self-consistent and green and S1b would be the slice that exploded.
//
// **What this suite is, and is not.** It checks that every *observable* field
// the derivation produces is present in what the desktop panel actually
// renders. That is a coarse claim for the text fields, on purpose: it catches a
// rule that was **understood wrongly**, not a field that was left out and not a
// field that landed in the wrong carrier.
//
//  - **A dropped field is not this suite's job.** The text assertions are
//    occurrence *lower bounds*, and deleting a derived field only removes an
//    obligation — the page still renders, and every remaining bound still
//    holds. Fields being present at all is pinned by the unit tests in
//    `core/view/*.test.ts`, whose §5.1 / §5.2 mutations have been run and go
//    red there.
//  - **A misplaced field is not this suite's job either.** Whether each field
//    lands in **its own** carrier — this row's title inside this row's element
//    and not borrowed from a neighbouring badge — is the faithful-projection
//    property, and it is `checkProjection`'s job in S1b, against markers this
//    page does not carry yet. Do not read a green run here as "the projection
//    is verified".
//
// **Covered.** Five text fields, counted against `textContent` only:
// `module.title`, `module.empty` (zero-row modules only), `row.title`,
// `row.kind` (when non-null), `badge.text`.
//
// Separately, two fields of `row.status`. These are **not** lower bounds but
// exact equalities against the carrier the page already marks
// (`[data-nc-task-status]`, `public.tsx:725-733`):
// `status.phrase` against that node's `title` (`:731` is the bare phrase) and
// `status.token` against the `data-nc-task-status` attribute (`:729`). A row
// whose derived `status` is null must have no such node.
//
// **Attributes are not "observable text" here.** An earlier version folded
// every `aria-label` / `title` in the subtree into one blob and counted
// occurrences in it; that let a derived field pass by being a substring of some
// *renderer-authored* sentence. Two mutations were run against it and both
// stayed green: `taskStatusPhrase` returning the bare `status` (the desktop's
// `Status: running — step 2 of 3` still satisfied a lower bound of one), and
// the derivation prefixing its own `phrase` with `Status: ` (which would make a
// faithful painter write `Status: Status: running — step 2 of 3`). Counting the
// text fields in `textContent` and pinning the status to its carrier exactly is
// what closes both.
//
// **Excluded, and why each exclusion is somebody's job.**
//
//  - **Id-shaped fields — excluded and must stay excluded.** `row.id` (a
//    `card.id` / `task.blockId`) and every action *payload* reach only React
//    keys and callbacks (`public.tsx:516,638`), so asserting them would fail
//    against a correct page.
//  - **Action labels — excluded, and this is S1b's opening item.** The page
//    writes four sentences that no `RowAction` carries: `Delete card ${…}`
//    (`:536`), `Delete card` (`:537`), `Show ${task.key} in the report`
//    (`:642`), `Open the worker card for ${task.key}` (`:747`). `RowAction` is
//    a `kind` plus an id and has no label field, so **S1b's two painters will
//    each re-invent all four** — which is precisely the failure mode
//    `taskStatusPhrase` was in until this slice, one wording written twice and
//    drifting. Naming an action is wording, and wording belongs in `core`.
//  - **`badge.struck` — excluded, S1b's.** It is only a class difference
//    (`taskWithdrawn` vs `taskNote`, `:663-665`); neither `textContent` nor any
//    marker this page carries distinguishes them.
//  - **The set and order of a row's actions — excluded, S1b's.** `rowFields`
//    does not read `row.actions` at all, so reordering a row's actions — or
//    adding and dropping one — leaves this suite green. That `actions` is a
//    checked sequence (`core/view/panel.ts`, `PanelRow.actions`) is a claim
//    only S1b's `checkProjection` carries, against the action markers this page
//    does not have.
//  - **The DOM order of the modules — excluded, S1b's.** Each module is located
//    by its own static selector (`renderedRows`) and asserted independently, so
//    swapping the order of `view.rowModules` — or of the two module elements on
//    the page — is invisible here. "Cards before Tasks is part of the view
//    model" (`core/view/wave-page.ts`, `deriveWavePageView`) is likewise
//    S1b's `checkProjection` to hold, once `paintPanel` walks the sequence.

import { describe, expect, it } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/wave.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveWavePageView } from '../../../../../core/view/wave-page.ts';
import { card, renderPage } from './test-fixtures.tsx';

/*
 * The fixture has to give the assertion teeth, which is a fixture requirement
 * and not a rendering one:
 *
 *  - a titled card **and** an untitled one, so `row.kind` is exercised on both
 *    sides of its condition;
 *  - the untitled card is kernel-owned, so its row prints its kind exactly once
 *    and a derivation that emitted `kind` unconditionally would be asking the
 *    row for a second occurrence that is not there;
 *  - a task with a `statusDetail`, so `phrase !== token` and dropping the join
 *    is visible;
 *  - a withdrawn task, for a struck declaration badge;
 *  - a task with both a kind and a worker card.
 *
 * Titles, kinds and badge texts are chosen not to be substrings of one another,
 * so an occurrence of one cannot stand in for another.
 */
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
  },
];

/** Where each module's rows live in the desktop panel today. The two selectors
 *  are spelled out as literals rather than looked up: `no-class-dom-query`
 *  requires a static selector, and a dynamic one would fail closed anyway. */
function renderedRows(container: Element, key: RowModuleView['key']): readonly Element[] {
  return key === 'cards'
    ? [...container.querySelectorAll('[data-nc-card-inventory] > li')]
    : [...container.querySelectorAll('[data-nc-task-inventory] > li')];
}

/** What a reader sees, and nothing a renderer says *about* it: `aria-label` and
 *  `title` are the page's own chrome (`Status: `, `Delete card`, `Show ... in
 *  the report`), and letting a derived field match inside one of those
 *  sentences is how two real mutations stayed green. See the file head. */
function visibleText(root: Element): string {
  return root.textContent ?? '';
}

function occurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

/** The row's visible text fields, in no particular order — position is not what
 *  this suite claims. `status` is not among them: it has no text content at all
 *  on this surface, and is asserted exactly against its own carrier instead. */
function rowFields(row: PanelRow): readonly string[] {
  return [
    row.title,
    ...(row.kind !== null ? [row.kind] : []),
    ...row.badges.map((badge) => badge.text),
  ];
}

/**
 * The status, against the node the page already marks for it.
 *
 * Exact equality, not an occurrence bound: the phrase's carrier is `title`
 * (`public.tsx:731`), which is the bare phrase, and the token's carrier is the
 * `data-nc-task-status` attribute (`:729`). The dot's `aria-label` (`:730`) is
 * `Status: ${phrase}` and is deliberately **not** asserted — that prefix is
 * renderer chrome the view model does not own (`core/view/panel.ts`,
 * `RowStatus.phrase`), and asserting the label would license moving the prefix
 * into `core`, which is a mutation this suite has been shown to miss.
 */
function expectStatus(rowElement: Element, row: PanelRow, where: string): void {
  const dot = rowElement.querySelector('[data-nc-task-status]');
  if (row.status === null) {
    expect(dot, `${where}: derived no status, so the page must paint no dot`).toBeNull();
    return;
  }
  expect(dot, `${where}: status dot`).not.toBeNull();
  expect(dot?.getAttribute('title'), `${where}: phrase`).toBe(row.status.phrase);
  expect(dot?.getAttribute('data-nc-task-status'), `${where}: token`).toBe(row.status.token);
}

/**
 * Every field must be present **as many times as the derivation says it is**.
 *
 * A plain `toContain` per field is not enough here and the untitled card is
 * why: its derived `title` and a wrongly-derived `kind` would be the same
 * string, and one rendered occurrence would satisfy both. Multiplicity is what
 * makes "the page prints this fact twice" distinguishable from "once".
 */
function expectFieldsPresent(text: string, fields: readonly string[], where: string): void {
  const wanted = new Map<string, number>();
  for (const field of fields) wanted.set(field, (wanted.get(field) ?? 0) + 1);
  for (const [field, count] of wanted) {
    expect(occurrences(text, field), `${where}: ${JSON.stringify(field)} × ${count}`)
      .toBeGreaterThanOrEqual(count);
  }
}

describe('deriveWavePageView against the rendered desktop panel', () => {
  it('renders every derived module title, and every row field inside its own row', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS });
    const view = deriveWavePageView({ cards: CARDS, tasks: TASKS });
    const whole = visibleText(container);

    /* The fixture invariant the untitled-card arm's discriminating power rests
       on, asserted rather than assumed: `card-2` has no title, so its row
       prints its kind exactly **once** and an unconditional `kind` would be
       asking for a second occurrence that is not there. `deletable: false` is
       the other half — a deletable card would add a `Delete card harness`
       control, and while that label no longer enters the count (only
       `textContent` does), the fixture should not depend on that to stay
       discriminating. */
    expect(CARDS[1]).toMatchObject({ title: null, deletable: false });

    for (const module of view.rowModules) {
      expect(whole).toContain(module.title);
      /* Populated modules must not be printing their empty text. */
      expect(whole).not.toContain(module.empty);

      const rendered = renderedRows(container, module.key);
      expect(rendered.length, `${module.key}: rendered rows`).toBe(module.rows.length);

      module.rows.forEach((row, index) => {
        const element = rendered[index];
        const fields = rowFields(row);
        expectFieldsPresent(visibleText(element), fields, `${module.key}[${index}]`);
        expectStatus(element, row, `${module.key}[${index}]`);
        /* The coarse claim the slice's acceptance names: every field reaches
           the page at all. Kept alongside the scoped one so a regression that
           moves a field out of its row still reads differently from one that
           drops it. */
        for (const field of fields) expect(whole).toContain(field);
      });
    }
  });

  it('renders each module’s empty text when, and only when, the module has no rows', () => {
    const { container } = renderPage({ cards: [], tasks: [] });
    const view = deriveWavePageView({ cards: [], tasks: [] });
    const whole = visibleText(container);

    for (const module of view.rowModules) {
      expect(module.rows).toEqual([]);
      expect(whole).toContain(module.empty);
      expect(renderedRows(container, module.key).length).toBe(0);
    }
  });
});
