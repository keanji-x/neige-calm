// @vitest-environment jsdom
//
// #1234 S1b-3b — the **real page** as a faithful projection of its view model.
//
// Every earlier slice checked a painter in isolation. That is exactly the shape
// §6.10 names: a green synthetic painter says nothing about whether the page
// renders through it, and until this slice the page did not. So this suite
// renders the unmodified `WavePage` under its ordinary props contract, takes the
// **desktop panel subtree**, and runs `checkProjectionIn` over it.
//
// **Why the subtree and not the container.** `.mobileListSurface` and
// `.desktopPanelSurface` are siblings and are both in the DOM in production —
// the desktop side only takes `inert`. Today a whole-page scan happens to work
// because the mobile side carries no markers at all; the day S1b-4 marks it,
// the two would be read as one tree. The root is scoped now so that trap is
// never set.
//
// **What this suite is worth, and what carries the rest.** `checkProjectionIn`
// takes the painter on trust: it cannot prove this DOM came from that painter,
// and the painter built here is a second construction, not the page's. What
// turns a green run into evidence is **`desktop-entry.test.tsx`**, which mocks
// `paintDesktopPanel` and holds that the page calls it with the whole view and
// renders the value it hands back. **Its load-bearing assertions name no
// marker** — the call and view equality, the tag it plants in the painter's
// return value, and the fixture text that must vanish when the painter's output
// is replaced — so no spelling of a marker can go round those; only its
// module- and row-*count* assertions read `data-nc-module` / `data-nc-row`, and
// those are as spelling-bound as the scan below. See that file's head for the
// residue, and `tools/projection/public.ts`'s standing list for what nothing
// covers.
//
// The last describe below is a **second, narrower** guard, and its scope is
// worth being exact about: the page's source may spell none of `MARKER`'s
// attribute names, in either face. That stops the panel drifting back to being
// hand-composed by the cheapest route — someone retyping `data-nc-row` here —
// and it is **not** a proof that the painter ran. A marker reaches the DOM from
// this file with no literal in it at all: a computed property
// (`{...{[MARKER.module]: 'cards'}}`), a concatenation (`'data-' +
// 'nc-module'`), a marker-channel prop (`ui/panel-card` takes three, and
// legitimately spells the attribute names itself), or a component imported from
// a file that carries markers of its own. Read the describe as hygiene, and
// `desktop-entry.test.tsx` as the oracle.

import { cleanup, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/wave.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveWavePageView } from '../../../../../core/view/wave-page.ts';
import { checkProjectionIn } from '../../../../../tools/projection/public.ts';
import { makeDesktopPainter } from './desktop-painter.tsx';
import pageSource from './public.tsx?raw';
import { card, renderPage } from './test-fixtures.tsx';

afterEach(cleanup);

/*
 * The fixture, shaped to §3.5's oracle preconditions. The clauses that are
 * *possible on this surface* are asserted mechanically below; the two that are
 * not are named there rather than quietly dropped.
 */
const CARDS: readonly CardWire[] = [
  /* Titled and deletable: `title !== kind`, a `kind` carrier of its own, and
     with `onDeleteCard` passed, both of `delete-card`'s sentences. */
  card({ id: 'card-1', kind: 'terminal', title: 'Build log', deletable: true }),
  /* Untitled and kernel-owned: `kind === null`, one badge, no delete. */
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

const TASKS: readonly ReportTaskRow[] = [
  /* A dispatched task with a reason: `phrase !== token`, a kind *and* a worker
     card, so the kind is a control and carries `open-card`. */
  {
    blockId: 'block-1', key: 'alpha-gate', state: 'ready', declaration: null,
    status: 'running', statusDetail: 'step 2 of 3', kind: 'codex', workerCardId: 'card-1',
  },
  /* Withdrawn: a struck declaration badge, no kind, no status. */
  {
    blockId: 'block-2', key: 'beta-gate', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null,
  },
  /* `status === ''` — a legitimate state upstream (`core/domain/report.ts`), and
     the one that makes an empty token's text obligation vacuously true. A
     checker comparing the token by truthiness would never be caught without it.
     The kind is present with no worker card, so it renders as a label and the
     row carries only `reveal-block`. */
  {
    blockId: 'block-3', key: 'gamma-gate', state: 'ready', declaration: 'Not ready',
    status: '', statusDetail: null, kind: 'claude', workerCardId: null,
  },
];

/** The painter the page builds for this render, rebuilt here. The duplication is
 *  the trust boundary named in the file head; `desktop-entry.test.tsx` closes
 *  it by holding the call, not the marker spelling. */
const painter = () => makeDesktopPainter({
  onOpenCard: vi.fn(), onOpenTask: vi.fn(), onDeleteCard: vi.fn(),
});

/** The desktop panel subtree — not the container. */
function desktopPanel(container: Element): Element {
  const root = container.querySelector('[data-nc-desktop-panel]');
  expect(root, 'the desktop panel surface must be findable').not.toBeNull();
  return root!;
}

// ── The fixture shape guard ──────────────────────────────────────────────────

const ALL_MODULES: readonly RowModuleView[] = [
  ...deriveWavePageView({ cards: CARDS, tasks: TASKS }).rowModules,
  ...deriveWavePageView({ cards: [], tasks: [] }).rowModules,
];
const ALL_ROWS: readonly PanelRow[] = ALL_MODULES.flatMap((module) => [...module.rows]);

describe('fixture shape guard', () => {
  it('title and kind are non-empty and neither contains the other', () => {
    for (const row of ALL_ROWS) {
      expect(row.title).not.toEqual('');
      if (row.kind === null) continue;
      expect(row.kind).not.toEqual('');
      expect(row.title.includes(row.kind)).toBe(false);
      expect(row.kind.includes(row.title)).toBe(false);
    }
  });

  it('kind is exercised both null and non-null', () => {
    expect(ALL_ROWS.some((row) => row.kind === null)).toBe(true);
    expect(ALL_ROWS.some((row) => row.kind !== null)).toBe(true);
  });

  it('status is exercised null, non-null, phrase !== token, and an empty token', () => {
    expect(ALL_ROWS.some((row) => row.status === null)).toBe(true);
    expect(ALL_ROWS.some((row) => row.status !== null)).toBe(true);
    expect(ALL_ROWS.some((row) => row.status !== null && row.status.phrase !== row.status.token)).toBe(true);
    expect(ALL_ROWS.some((row) => row.status !== null && row.status.token === '')).toBe(true);
  });

  /*
   * **Two of S1b-2's clauses are unreachable here, and are recorded rather than
   * silently dropped.** `deriveWavePageView` gives a Cards row at most one badge
   * (`kernel-owned`) and a Task row at most one (the declaration), so "more than
   * one badge" and "two badges with the same text and different ids" cannot be
   * built out of real props at all. They stay covered by the synthetic painters
   * in `projection-contract.test.tsx`, which is the right place for a shape the
   * production derivation cannot produce. What is asserted here is the part that
   * *is* reachable, in both directions.
   */
  it('badge counts cover zero and one, which is the whole reachable range', () => {
    const counts = ALL_ROWS.map((row) => row.badges.length);
    expect(counts).toContain(0);
    expect(counts).toContain(1);
    expect(Math.max(...counts)).toBe(1);
  });

  it('every action kind the desktop offers appears at least once', () => {
    const painted = ALL_ROWS.flatMap((row) => row.actions.map((action) => action.kind));
    for (const kind of ['reveal-block', 'open-card', 'delete-card']) expect(painted).toContain(kind);
  });

  it('every module key is exercised both empty and non-empty', () => {
    for (const key of ['cards', 'tasks'] as const) {
      const mine = ALL_MODULES.filter((module) => module.key === key);
      expect(mine.some((module) => module.rows.length === 0)).toBe(true);
      expect(mine.some((module) => module.rows.length > 0)).toBe(true);
    }
  });
});

// ── The projection itself ────────────────────────────────────────────────────

describe('the rendered desktop panel projects its view model faithfully', () => {
  it('with cards and tasks, a delete handler, and every row shape above', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const view = deriveWavePageView({ cards: CARDS, tasks: TASKS });
    expect(checkProjectionIn(painter(), view.rowModules, desktopPanel(container))).toEqual([]);
  });

  it('with both modules empty', () => {
    const { container } = renderPage({ cards: [], tasks: [] });
    const view = deriveWavePageView({ cards: [], tasks: [] });
    expect(checkProjectionIn(painter(), view.rowModules, desktopPanel(container))).toEqual([]);
  });

  /*
   * The capability table is a per-render fact, so the page must be checked in
   * both of its states: with no `onDeleteCard`, `paintModule` filters every
   * `delete-card` away and the page must paint no × — and the *painter under
   * test* must be the one built without the handler, or the check would be
   * asking for a control the page correctly did not draw.
   */
  it('with no delete handler, so no row carries a delete action', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS });
    const view = deriveWavePageView({ cards: CARDS, tasks: TASKS });
    const noDelete = makeDesktopPainter({ onOpenCard: vi.fn(), onOpenTask: vi.fn() });
    expect(checkProjectionIn(noDelete, view.rowModules, desktopPanel(container))).toEqual([]);
    /* Not vacuous: the derivation really did offer a `delete-card` that the
       capability table removed. */
    expect(view.rowModules[0].rows.some((row) =>
      row.actions.some((action) => action.kind === 'delete-card'))).toBe(true);
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
  });

  /* The rows really are on the page these assertions inspect — a projection over
     an empty subtree is green for the wrong reason. */
  it('is not vacuous: the marked rows are in the desktop subtree', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);
    /* Static selectors — `no-class-dom-query` requires them — pinned to `MARKER`
       on the spot so the two literals cannot drift from the table. */
    expect(MARKER.row).toBe('data-nc-row');
    expect(MARKER.module).toBe('data-nc-module');
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length + TASKS.length);
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(2);
  });
});

// ── A hygiene guard beside the above, not a premise of it ────────────────────

describe('marker-literal hygiene guard: the page spells no MARKER name', () => {
  /*
   * A hygiene guard, not the entry oracle — see the file head for what it does
   * and does not close, and `desktop-entry.test.tsx` for the claim that the
   * page goes through the painter at all.
   *
   * Both spellings. Δ5's lesson, paid for once already: an oracle bound to one
   * spelling of an attribute reports zero while `dataset.ncRow` sits in the
   * file, because a DOM attribute has a kebab face (`getAttribute`, CSS) and a
   * camel one (`dataset`). Binding to the concept means scanning for both.
   *
   * The names come from `MARKER` rather than being retyped, so a marker added to
   * that table is scanned for here without anyone remembering to.
   */
  const attributeNames: readonly string[] = Object.values(MARKER);

  const camel = (attribute: string): string =>
    attribute.replace(/^data-/, '').replace(/-([a-z])/g, (_all, letter: string) => letter.toUpperCase());

  it('spells none of MARKER’s attribute names', () => {
    for (const name of attributeNames) {
      expect(pageSource, `public.tsx must not spell ${name}`).not.toContain(name);
    }
  });

  it('spells none of them in their dataset form either', () => {
    for (const name of attributeNames) {
      const property = camel(name);
      expect(property.startsWith('nc'), 'the camel form should be a dataset property').toBe(true);
      expect(pageSource, `public.tsx must not spell ${property}`).not.toMatch(
        new RegExp(`\\b${property}\\b`),
      );
    }
  });

  /* The scan is over real source: a mis-wired import would make every assertion
     above vacuously true. */
  it('is scanning this page’s source', () => {
    expect(pageSource).toContain('export function WavePage');
    expect(pageSource).toContain('paintDesktopPanel');
  });
});
