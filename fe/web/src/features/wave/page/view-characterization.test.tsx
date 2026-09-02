// Characterization: `deriveWavePageView` against the panel as it stands.
//
// #1234 S1a derives the wave panel's view model in `core/view` but wires
// nothing — `public.tsx` is untouched by this slice, deliberately, because this
// suite uses it as the oracle. If the derivation misread the desktop panel (the
// `kind` condition, the ownership badge, the `statusDetail` join), S1a would be
// self-consistent and green and S1b would be the slice that exploded.
//
// **What this suite is, and is not.** It checks that the **S1a text and status
// fields** the derivation produces are present in what the desktop panel
// actually renders. That is a coarse claim for the text fields, on purpose: it
// catches a rule that was **understood wrongly**, not a field that was left out
// and not a field that landed in the wrong carrier.
//
// It is **not** "every observable field". Since S1b-1, `RowAction` carries
// `label` and `hint` — two more observable fields, in `aria-label` / `title`.
// Those **are** covered here, but only as *row-scoped membership*: each derived
// sentence must equal one of the attribute values the row's own subtree
// carries. Which element carries it is still S1b-2's `checkProjection` against
// S1b-3's markers; see the exclusions below.
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
//  - **Action wording — covered since S1b-1, as row-scoped membership.** The
//    page writes four sentences: `Delete card ${…}` (`:536`), `Delete card`
//    (`:537`), `Show ${task.key} in the report` (`:642`), `Open the worker card
//    for ${task.key}` (`:747`). As of S1b-1 `RowAction` carries all four in its
//    `label` / `hint` fields — the wording moved into `core` for the same reason
//    `taskStatusPhrase` did, so that S1b's two painters do not each re-invent
//    it. S1b-1 does **not** touch `public.tsx`, so the tree now holds two copies
//    of each sentence, which is exactly the drift class #1234 exists to remove.
//    `expectActionWording` below is what keeps the two copies pinned together
//    until S1b-3 rewrites the page: for every action with a non-null `hint`, the
//    sentence must be **equal to** one of the `title` values in the row's own
//    subtree, and likewise `label` against `aria-label`. Three properties of
//    that check are load-bearing and none of them is decoration:
//      * it is set membership (`toContain` over an **array** of attribute
//        values), not a substring test on a joined blob — the joined-blob form
//        is what let two real mutations stay green (see the paragraph above);
//      * it is scoped to the row's `<li>`, so a sentence rendered for a
//        *different* row cannot stand in;
//      * it is *not* an exhaustive projection. The page carries no
//        `data-nc-row-action` markers yet (S1b-3 adds them), so the check cannot
//        say *which element* carries which sentence, nor that the row carries no
//        extra action. That is `checkProjection`'s job in S1b-2/3, and a green
//        run here must not be read as "the projection is verified".
//  - **The null side of `label` / `hint` — not covered, deliberately.** A Cards
//    row's `open-card` derives `label: null, hint: null` ("the derivation
//    invented no wording for the row body", `wave-page.ts`'s `cardRow`). No
//    assertion here holds that: the row's subtree carries other `title` /
//    `aria-label` values (the delete control's, and on Task rows the status
//    dot's), so "no such attribute exists in this row" would be false against a
//    correct page, and any weaker phrasing would not be checking the null. The
//    claim that a null wording stays null is `core/view/wave-page.test.ts`'s;
//    the claim that a painter emits no attribute for it is S1b-2/3's.
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

import { describe, expect, it, vi } from 'vitest';

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
 *  - the titled card is deletable **and** the render passes `onDeleteCard`, so
 *    the page actually paints the × (`public.tsx:514` needs both halves) and
 *    `delete-card`'s two sentences have a carrier to be found in. Without the
 *    callback the control is not drawn at all and the assertion would be
 *    vacuous;
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

/** What a reader sees, and nothing a renderer says *about* it: `textContent`
 *  only, never `aria-label` / `title`. Letting a derived text field match
 *  inside one of those attribute sentences is how two real mutations stayed
 *  green (see the file head), so they are out of the count.
 *
 *  Note what that exclusion no longer means. `Status: ` is still renderer
 *  chrome, but since S1b-1 `Delete card ${…}` / `Delete card` / `Show … in the
 *  report` / `Open the worker card for …` are **`RowAction.label` / `.hint`,
 *  owned by `core`**. They stay out of *this* count because they are not
 *  visible text — `expectActionWording` checks them against the attributes that
 *  actually carry them instead. */
function visibleText(root: Element): string {
  return root.textContent ?? '';
}

/** Every value of `attribute` inside the row's subtree, the row element
 *  included, as a **list of whole values** — never joined. Membership in this
 *  list is an exact string equality against one rendered attribute; a joined
 *  string would turn the same assertion into a substring test, which is the
 *  shape two earlier mutations survived.
 *
 *  The two selectors are spelled as literals and chosen by a branch, for the
 *  same reason `renderedRows` does it: `no-class-dom-query` requires a static
 *  selector. */
function attributeValues(root: Element, attribute: 'title' | 'aria-label'): readonly string[] {
  const carriers = attribute === 'title'
    ? root.querySelectorAll('[title]')
    : root.querySelectorAll('[aria-label]');
  const values: string[] = [];
  for (const element of [root, ...carriers]) {
    const value = element.getAttribute(attribute);
    if (value !== null) values.push(value);
  }
  return values;
}

/**
 * The row's derived action wording, against the row's own attribute values.
 *
 * Row-scoped membership, not projection: see the file head. `null` sides are
 * not asserted — the Cards row body's `open-card` is `{label: null, hint:
 * null}` and no assertion here covers that.
 */
function expectActionWording(rowElement: Element, row: PanelRow, where: string): void {
  const titles = attributeValues(rowElement, 'title');
  const labels = attributeValues(rowElement, 'aria-label');
  row.actions.forEach((action, index) => {
    const at = `${where}.actions[${index}] (${action.kind})`;
    if (action.hint !== null) expect(titles, `${at}: hint`).toContain(action.hint);
    if (action.label !== null) expect(labels, `${at}: label`).toContain(action.label);
  });
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
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const view = deriveWavePageView({ cards: CARDS, tasks: TASKS });
    const whole = visibleText(container);

    /* The fixture invariant the untitled-card arm's discriminating power rests
       on, asserted rather than assumed: `card-2` has no title, so its row
       prints its kind exactly **once** and an unconditional `kind` would be
       asking for a second occurrence that is not there. `deletable: false` is
       the other half — the render now passes `onDeleteCard`, so a deletable
       `card-2` would paint a × carrying `Delete card harness`; that sentence is
       an `aria-label` and so cannot enter the `textContent` count, but the
       fixture should not depend on that to stay discriminating, and keeping the
       row control-free keeps the arm's teeth independent of which carrier each
       assertion reads. */
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
        expectActionWording(element, row, `${module.key}[${index}]`);
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
