// Characterization: `deriveWavePageView` against the panel as it stands.
//
// #1234 S1a derives the wave panel's view model in `core/view` but wires
// nothing — `public.tsx` is untouched by this slice, deliberately, because this
// suite uses it as the oracle. If the derivation misread the desktop panel (the
// `kind` condition, the ownership badge, the `statusDetail` join), S1a would be
// self-consistent and green and S1b would be the slice that exploded.
//
// **What this suite is, and is not.** It checks that every *observable text
// field* the derivation produces is present in the text the desktop panel
// actually renders. That is a coarse claim, on purpose: it catches a
// misunderstood rule, not a misplaced one. Whether each field lands in **its
// own** carrier — this row's title inside this row's element and not borrowed
// from a neighbouring badge — is the faithful-projection property, and it is
// `checkProjection`'s job in S1b, against markers this page does not carry yet.
// Do not read a green run here as "the projection is verified".
//
// The observable set is closed and is six fields: `module.title`,
// `module.empty` (zero-row modules only), `row.title`, `row.kind` (when
// non-null), `badge.text`, `status.phrase`. **Id-shaped fields are excluded and
// must stay excluded**: `row.id` (a `card.id` / `task.blockId`) and every
// action payload reach only React keys and callbacks (`public.tsx:516,638`), so
// asserting them would fail against a correct page.
//
// `status.phrase` is not text content on this surface: the desktop status dot
// is an empty span that carries the phrase in `aria-label` and `title`
// (`public.tsx:728-733`). "Observable text" here therefore means text content
// plus those two attributes, joined by a separator no field can span.

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

const SEPARATOR = '\u0000';

function observableText(root: Element): string {
  const parts = [root.textContent ?? ''];
  for (const element of [root, ...root.querySelectorAll('*')]) {
    for (const attribute of ['aria-label', 'title']) {
      const value = element.getAttribute(attribute);
      if (value !== null) parts.push(value);
    }
  }
  return parts.join(SEPARATOR);
}

function occurrences(haystack: string, needle: string): number {
  return haystack.split(needle).length - 1;
}

/** The row's observable text fields, in no particular order — position is not
 *  what this suite claims. */
function rowFields(row: PanelRow): readonly string[] {
  return [
    row.title,
    ...(row.kind !== null ? [row.kind] : []),
    ...row.badges.map((badge) => badge.text),
    ...(row.status !== null ? [row.status.phrase] : []),
  ];
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
    const whole = observableText(container);

    for (const module of view.rowModules) {
      expect(whole).toContain(module.title);
      /* Populated modules must not be printing their empty text. */
      expect(whole).not.toContain(module.empty);

      const rendered = renderedRows(container, module.key);
      expect(rendered.length, `${module.key}: rendered rows`).toBe(module.rows.length);

      module.rows.forEach((row, index) => {
        const element = rendered[index];
        const fields = rowFields(row);
        expectFieldsPresent(observableText(element), fields, `${module.key}[${index}]`);
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
    const whole = observableText(container);

    for (const module of view.rowModules) {
      expect(module.rows).toEqual([]);
      expect(whole).toContain(module.empty);
      expect(renderedRows(container, module.key).length).toBe(0);
    }
  });
});
