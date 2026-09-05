// Same-source regression: `deriveTrackPageView` against the panel that is now
// rendered *from* it.
//
// **This file has lost a property, and the loss is recorded here rather than
// papered over.** In #1234 S1a this was an **independent oracle**: the
// derivation was new, `public.tsx` was deliberately untouched, and the page was
// a second, hand-written expression of the same wording. Comparing them caught
// a rule that had been *understood wrongly* — the `kind` condition, the
// ownership badge, the `statusDetail` join — because the two sides could
// disagree.
//
// Since S1b-3b they cannot. The page calls `deriveTrackPageView` itself and
// paints the result through `desktop-painter.tsx`, so both sides of every
// comparison below come from the same derivation. A derivation that misreads
// the panel now produces a page that misreads it identically, and this suite
// stays green. That is not a defect in the wiring — it is what wiring the page
// to one source *means* — but the independence is gone and no comment here may
// keep claiming it.
//
// **What it still guards, and what now guards the rest.**
//
//  - Still here: the derived fields really reach the rendered DOM, in **at
//    least** the derived multiplicity, inside their own row — a same-source
//    regression against the *rendering path*. (`expectFieldsPresent` compares
//    with `>=`, so a field printed more often than the view model says is not
//    red here; the exact-carrier question is `checkProjection`'s.) A painter or
//    a page that drops a derived field, prints
//    it once where the view model says twice, or renders a populated module's
//    empty text goes red here, because the derivation and the DOM disagree even
//    though the derivation is shared.
//  - Not here any more: any check on whether the derivation reads the product
//    correctly. That is `core/view/track-page.test.ts` and
//    `core/view/panel.test.ts`, whose §5.1 / §5.2 mutations have been run.
//  - Not here: whether the page is a faithful *projection* — each field in its
//    own carrier, the exact set and order of a row's actions, module order.
//    That is `desktop-projection.test.tsx`, which runs `checkProjectionIn` over
//    the real rendered page, with `desktop-entry.test.tsx` holding that the page
//    goes through the painter at all.
//  - Not here: what the user can actually do. The three actions' payloads and
//    callbacks, and the delete control's presence, are behaviour and are
//    asserted as behaviour in `public.test.tsx`.
//
// **What the assertions are, mechanically.** The **S1a text and status fields**
// the derivation produces must be present in what the desktop panel renders.
// That is a coarse claim for the text fields, on purpose: it catches a field
// that never reached the DOM, not a field that landed in the wrong carrier.
//
// Since S1b-1, `RowAction` carries `label` and `hint` — two more observable
// fields, in `aria-label` / `title`. Those **are** covered here, but only as
// *row-scoped membership*: each derived sentence must equal one of the
// attribute values the row's own subtree carries. Which element carries it is
// `checkProjection`'s question, not this file's.
//
//  - **A dropped field is not this suite's job.** The text assertions are
//    occurrence *lower bounds*, and deleting a derived field only removes an
//    obligation — the page still renders, and every remaining bound still
//    holds. Fields being present at all is pinned by the unit tests in
//    `core/view/*.test.ts`.
//  - **A misplaced field is not this suite's job either.** Whether each field
//    lands in **its own** carrier — this row's title inside this row's element
//    and not borrowed from a neighbouring badge — is the faithful-projection
//    property, and it is `checkProjectionIn`'s in `desktop-projection.test.tsx`.
//    Do not read a green run here as "the projection is verified".
//
// **Covered.** Five text fields, counted against `textContent` only:
// `module.title`, `module.empty` (zero-row modules only), `row.title`,
// `row.kind` (when non-null), `badge.text`.
//
// Separately, two fields of `row.status`. These are **not** lower bounds but
// exact equalities against the carrier the panel marks — the compact status
// word painted by `desktop-painter.tsx`: `status.phrase` against that node's
// `title`, and `status.token` against its `data-nc-status` attribute. A row
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
// **The principal gaps, and whose job each is.** Read this as the main gaps,
// **not** as an exhaustive list of everything a green run fails to say — the
// same-source note at the top is the largest one, the two paragraphs above name
// two more, and there are smaller ones called out inline below.
//
//  - **Id-shaped fields — excluded and must stay excluded.** `row.id` (a
//    `card.id` / `task.blockId`) and every action *payload* reach only React
//    keys and callbacks, so asserting them against the DOM would fail against a
//    correct page. That the payloads reach the right callback is
//    `public.test.tsx`'s, as behaviour.
//  - **Action wording — covered since S1b-1, as row-scoped membership.** Four
//    sentences reach the panel: `Delete card ${…}`, `Delete card`, `Show ${key}
//    in the report`, `Open the worker card for ${key}`. As of S1b-1 `RowAction`
//    carries all four in its `label` / `hint` fields, and since S1b-3b the
//    painter is their only writer — the page no longer holds a second copy, so
//    what `expectActionWording` pins is the derivation against the painter
//    rather than two hand-written copies against each other. Three properties
//    of that check are load-bearing and none of them is decoration:
//      * it is set membership (`toContain` over an **array** of attribute
//        values), not a substring test on a joined blob — the joined-blob form
//        is what let two real mutations stay green (see the paragraph above);
//      * it is scoped to the row's `<li>`, so a sentence rendered for a
//        *different* row cannot stand in;
//      * it is *not* an exhaustive projection: it cannot say *which element*
//        carries which sentence, nor that the row carries no extra action.
//        That is `checkProjectionIn`'s against the painter's markers, in
//        `desktop-projection.test.tsx`.
//  - **The null side of `label` / `hint` — not covered, deliberately.** A Cards
//    row's `open-card` derives `label: null, hint: null` ("the derivation
//    invented no wording for the row body", `track-page.ts`'s `cardRow`). No
//    assertion here holds that: the row's subtree carries other `title` /
//    `aria-label` values (the delete control's, and on Task rows the status
//    carrier's), so "no such attribute exists in this row" would be false against a
//    correct page, and any weaker phrasing would not be checking the null. The
//    claim that a null wording stays null is `core/view/track-page.test.ts`'s;
//    the claim that a painter emits no attribute for it is the projection's.
//  - **`badge.struck` — excluded.** It is only a class difference
//    (`taskWithdrawn` vs `taskNote` in `desktop-painter.tsx`); neither
//    `textContent` nor any marker distinguishes them here.
//  - **The exact set and order of a row's actions — excluded.** There is no
//    exact-set or order guarantee here: `expectActionWording` iterates
//    `row.actions` and checks membership per action, which pins nothing about
//    the sequence as a whole. Note what that iteration *does* catch, so the gap
//    is not read as wider than it is — adding an action whose `label` or `hint`
//    is a sentence the panel does not carry goes **red**. The mutations that
//    survive are the four where no new obligation appears: reordering a row's
//    actions, deleting an action, adding one with `label` and `hint` both null,
//    and adding one that reuses wording the row's subtree already renders. That
//    `actions` is a checked sequence (`core/view/panel.ts`, `PanelRow.actions`)
//    is `checkProjectionIn`'s claim, against the action markers.
//  - **The DOM order of the modules — excluded.** Each module is located by its
//    own static selector (`renderedRows`) and asserted independently, so
//    swapping the order of `view.rowModules` — or of the two module elements on
//    the page — is invisible here. "Cards before Tasks is part of the view
//    model" (`core/view/track-page.ts`, `deriveTrackPageView`) is
//    `checkProjectionIn`'s to hold, now that `paintPanel` walks the sequence.

import { describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/track.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { card, renderPage } from './test-fixtures.tsx';

/*
 * The fixture has to give the assertion teeth, which is a fixture requirement
 * and not a rendering one:
 *
 *  - a titled card **and** an untitled one, so `row.kind` is exercised on both
 *    sides of its condition;
 *  - the titled card is deletable **and** the render passes `onDeleteCard`, so
 *    the panel actually paints the × — `card.deletable` decides whether
 *    `cardRow` derives a `delete-card` at all and `onDeleteCard !== undefined`
 *    is the desktop painter's capability table, and both halves are needed —
 *    and
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

/** The row's visible text fields, in no particular order — position is not what
 *  this suite claims. `status` is not among them: it has no text content at all
 *  on this surface, and is asserted exactly against its own carrier instead.
 *
 *  Of a badge this reads `text` only — a further gap the file head's list does
 *  not enumerate: `badge.id` is never asserted here, and since these are
 *  occurrence lower bounds there is no claim about the row's **exact** set of
 *  badges either. Both are S1b's `checkProjection`. */
function rowFields(row: PanelRow): readonly string[] {
  return [
    row.title,
    ...(row.kind !== null ? [row.kind] : []),
    ...row.badges.map((badge) => badge.text),
  ];
}

/**
 * The status, against the node the painter marks for it.
 *
 * Exact equality, not an occurrence bound. All three carriers are written by
 * `desktop-painter.tsx`'s `statusDot`: the phrase's is `title`, which is the
 * phrase; the token's is the `data-nc-status` attribute. The status word is
 * `aria-hidden` because the same phrase is already the reveal control's
 * accessible description.
 */
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

describe('deriveTrackPageView against the rendered desktop panel', () => {
  it('renders every derived module title, and every row field inside its own row', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const view = deriveTrackPageView({ cards: CARDS, tasks: TASKS });
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
    const view = deriveTrackPageView({ cards: [], tasks: [] });
    const whole = visibleText(container);

    for (const module of view.rowModules) {
      expect(module.rows).toEqual([]);
      expect(whole).toContain(module.empty);
      expect(renderedRows(container, module.key).length).toBe(0);
    }
  });
});
