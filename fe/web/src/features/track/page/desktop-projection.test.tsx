// @vitest-environment jsdom
//
// The real page as a faithful projection of its view model, checked over the desktop panel subtree: the mobile surface is a sibling in the same DOM.

import { cleanup, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { checkProjectionIn } from '../../../../../tools/projection/public.ts';
import { makeDesktopPainter } from './desktop-painter.tsx';
import pageSource from './public.tsx?raw';
import { card, openableCardsOf, renderPage } from './test-fixtures.tsx';

afterEach(cleanup);

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
    status: 'running', statusDetail: 'step 2 of 3', kind: 'codex', workerCardId: 'card-1', pendingReason: null,
  },
  /* Withdrawn: a struck declaration badge, no kind, no status. */
  {
    blockId: 'block-2', key: 'beta-gate', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
  /* `status === ''`: unreachable through today's derivation but admitted by the row type; a checker comparing the token by truthiness would never be caught without it. */
  {
    blockId: 'block-3', key: 'gamma-gate', state: 'ready', declaration: 'Not ready',
    status: '', statusDetail: null, kind: 'claude', workerCardId: null, pendingReason: null,
  },
];

/** The painter the page builds for this render, rebuilt here; `desktop-entry.test.tsx` holds the call. */
const painter = () => makeDesktopPainter({
  onOpenCard: vi.fn(), onOpenTask: vi.fn(), onDeleteCard: vi.fn(),
});

/** The desktop panel subtree — not the container. */
function desktopPanel(container: Element): Element {
  const root = container.querySelector('[data-nc-desktop-panel]');
  expect(root, 'the desktop panel surface must be findable').not.toBeNull();
  return root!;
}

const ALL_MODULES: readonly RowModuleView[] = [
  ...deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) }).rowModules,
  ...deriveTrackPageView({ cards: [], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() }).rowModules,
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

describe('the rendered desktop panel projects its view model faithfully', () => {
  it('with cards and tasks, a delete handler, and every row shape above', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const view = deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) });
    expect(checkProjectionIn(painter(), view.rowModules, desktopPanel(container))).toEqual([]);
  });

  it('with both modules empty', () => {
    const { container } = renderPage({ cards: [], tasks: [] });
    const view = deriveTrackPageView({ cards: [], tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: new Set() });
    expect(checkProjectionIn(painter(), view.rowModules, desktopPanel(container))).toEqual([]);
  });

  it('with no delete handler, so no row carries a delete action', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS });
    const view = deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) });
    const noDelete = makeDesktopPainter({ onOpenCard: vi.fn(), onOpenTask: vi.fn() });
    expect(checkProjectionIn(noDelete, view.rowModules, desktopPanel(container))).toEqual([]);
    expect(view.rowModules[0].rows.some((row) =>
      row.actions.some((action) => action.kind === 'delete-card'))).toBe(true);
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
  });

  it('is not vacuous: the marked rows are in the desktop subtree', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);
    expect(MARKER.row).toBe('data-nc-row');
    expect(MARKER.module).toBe('data-nc-module');
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length + TASKS.length);
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(2);
  });
});

describe('marker-literal hygiene guard: the page spells no MARKER name', () => {
  /* Both spellings: a DOM attribute has a kebab face (`getAttribute`, CSS) and a camel one (`dataset`). */
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

  it('is scanning this page’s source', () => {
    expect(pageSource).toContain('export function TrackPage');
    expect(pageSource).toContain('paintDesktopPanel');
  });
});
