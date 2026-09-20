// @vitest-environment jsdom
//
// The real page's mobile panel as a faithful projection of its view model, one module at a time.

import { cleanup, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { activityLabelOf, type ActivityState } from '../../../../../core/domain/activity.ts';
import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import { checkProjectionIn } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter } from './mobile-painter.tsx';
import pageSource from './public.tsx?raw';
import { card, openableCardsOf, renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

const CARDS: readonly CardWire[] = [
  /* Titled: `title !== kind`, so both fields have a carrier of their own, and the derivation offers a `delete-card` for the capability table to filter away. */
  card({ id: 'card-1', kind: 'terminal', title: 'Build log', deletable: true }),
  /* Untitled and kernel-owned: `kind === null`, one badge. */
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

/** The painter the page builds for this render, rebuilt here; `mobile-entry.test.tsx` holds the call. */
const painter = () => makeMobilePainter({ onOpenTask: vi.fn(), backLabel: 'Report', onBack: vi.fn() });

/** The mobile panel subtree — not the container. */
function mobilePanel(container: Element): Element {
  const root = container.querySelector('[data-nc-mobile-panel]');
  expect(root, 'the mobile panel surface must be findable').not.toBeNull();
  return root!;
}

/* `openableCards` mirrors `renderPage`'s default so the two sides of each
   comparison below derive from one and the same input. */
const cardsModule = (cards: readonly CardWire[]) =>
  deriveTrackPageView({ cards, tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(cards, []) }).rowModules.filter((module) => module.key === 'cards');

/* The Tasks fixture, written as `deriveReportTasks` produces its rows; the shape guard re-reads the clauses off the derived rows. */
const TASKS: readonly ReportTaskRow[] = [
  /* Ready and undispatched: no declaration word, no run, a kind with no worker card, so the derivation offers `reveal-block` alone. */
  {
    blockId: 'block-1', key: 'alpha-impl', state: 'ready', declaration: null,
    status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
  },
  /* Dispatched, with a reason and a worker card: the status swallowed the readiness word, `phrase` is `status — detail`, and the derivation offers the `open-card` this surface refuses. */
  {
    blockId: 'block-2', key: 'beta-gate', state: 'not-ready', declaration: null,
    status: 'failed', statusDetail: 'track /tmp/alpha is not a git repository',
    kind: 'terminal', workerCardId: 'card-9', pendingReason: null,
  },
  /* Withdrawn: a struck declaration, and `kind === null` — upstream nulls the kind and the card id together for these rows. */
  {
    blockId: 'block-3', key: 'gamma-planner', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
  /* Unreadable: an ordinary, unstruck declaration beside the withdrawn one. */
  {
    blockId: 'block-4', key: 'delta-doc', state: 'unreadable', declaration: 'Unreadable',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
  /* Declared, not ready, never dispatched: the one row carrying a declaration badge and a kind at once. */
  {
    blockId: 'block-5', key: 'epsilon-fix', state: 'not-ready', declaration: 'Not ready',
    status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
  },
];

const tasksModule = (tasks: readonly ReportTaskRow[]): readonly RowModuleView[] =>
  deriveTrackPageView({ cards: [], tasks, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf([], tasks) }).rowModules.filter((module) => module.key === 'tasks');

const ROWS: readonly PanelRow[] = cardsModule(CARDS).flatMap((module) => [...module.rows]);

describe('fixture shape guard', () => {
  it('title and kind are non-empty and neither contains the other', () => {
    for (const row of ROWS) {
      expect(row.title).not.toEqual('');
      if (row.kind === null) continue;
      expect(row.kind).not.toEqual('');
      expect(row.title.includes(row.kind)).toBe(false);
      expect(row.kind.includes(row.title)).toBe(false);
    }
  });

  it('kind is exercised both null and non-null', () => {
    expect(ROWS.some((row) => row.kind === null)).toBe(true);
    expect(ROWS.some((row) => row.kind !== null)).toBe(true);
  });

  it('badge counts cover zero and one, which is the whole reachable range', () => {
    const counts = ROWS.map((row) => row.badges.length);
    expect(counts).toContain(0);
    expect(counts).toContain(1);
    expect(Math.max(...counts)).toBe(1);
  });

  it('the derivation offers both unsupported actions, so filtering them is not vacuous', () => {
    const offered = ROWS.flatMap((row) => row.actions.map((action) => action.kind));
    expect(offered).toContain('open-card');
    expect(offered).toContain('delete-card');
    for (const kind of offered) expect(painter().action[kind].supported).toBe(false);
  });

  it('carries no status at all, which is why the status clauses are absent above', () => {
    expect(ROWS.every((row) => row.status === null)).toBe(true);
  });
});

const TASK_ROWS: readonly PanelRow[] = tasksModule(TASKS).flatMap((module) => [...module.rows]);

describe('Tasks fixture shape guard', () => {
  it('exercises kind both null and non-null', () => {
    expect(TASK_ROWS.some((row) => row.kind === null)).toBe(true);
    expect(TASK_ROWS.some((row) => row.kind !== null)).toBe(true);
  });

  it('exercises status both null and non-null, with a phrase wider than its token', () => {
    expect(TASK_ROWS.some((row) => row.status === null)).toBe(true);
    const withStatus = TASK_ROWS.filter((row) => row.status !== null);
    expect(withStatus.length).toBeGreaterThan(0);
    for (const row of withStatus) {
      expect(row.status!.phrase).not.toEqual(row.status!.token);
      expect(row.status!.phrase.startsWith(row.status!.token)).toBe(true);
    }
  });

  it('exercises a struck declaration, an unstruck one, and a row with none', () => {
    const badges = TASK_ROWS.flatMap((row) => [...row.badges]);
    expect(badges.some((badge) => badge.struck)).toBe(true);
    expect(badges.some((badge) => !badge.struck)).toBe(true);
    expect(TASK_ROWS.some((row) => row.badges.length === 0)).toBe(true);
  });

  it('offers a supported action on every row and an unsupported one on some', () => {
    for (const row of TASK_ROWS) {
      expect(row.actions.map((action) => action.kind)).toContain('reveal-block');
    }
    const offered = TASK_ROWS.flatMap((row) => row.actions.map((action) => action.kind));
    expect(offered).toContain('open-card');
    expect(painter().action['reveal-block'].supported).toBe(true);
    expect(painter().action['open-card'].supported).toBe(false);
  });

  it('exercises a row carrying a declaration and a kind at once', () => {
    expect(TASK_ROWS.some((row) => row.badges.length > 0 && row.kind !== null)).toBe(true);
  });

  /* `Not ready` is not absent from the module — the undispatched row keeps it — so a blanket "never appears" would be false. */
  it('drops the readiness word for a ready row and for a dispatched one', () => {
    const words = TASK_ROWS.flatMap((row) => row.badges.map((badge) => badge.text));
    expect(words).not.toContain('Ready');
    const byId = new Map(TASK_ROWS.map((row) => [row.id, row]));
    expect(byId.get('block-1')!.badges).toEqual([]);
    expect(byId.get('block-2')!.badges).toEqual([]);
    expect(byId.get('block-2')!.status).not.toBeNull();
    expect(byId.get('block-5')!.badges.map((badge) => badge.text)).toEqual(['Not ready']);
  });
});

describe('the rendered mobile Cards page projects its view model faithfully', () => {
  it('with a titled row and an untitled kernel-owned one', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    expect(checkProjectionIn(painter(), cardsModule(CARDS), mobilePanel(container))).toEqual([]);
  });

  it('with no delete handler either', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards' });
    expect(checkProjectionIn(painter(), cardsModule(CARDS), mobilePanel(container))).toEqual([]);
  });

  it('with zero cards', () => {
    const { container } = renderPage({ cards: [], panel: 'cards' });
    expect(checkProjectionIn(painter(), cardsModule([]), mobilePanel(container))).toEqual([]);
  });

  it('is not vacuous: the marked rows are in the mobile subtree', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);
    expect(MARKER.row).toBe('data-nc-row');
    expect(MARKER.module).toBe('data-nc-module');
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length);
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
  });

  it('offers no card affordance: no delete, and the row is not a control', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);
    expect(root.querySelectorAll('[data-nc-row] button').length).toBe(0);
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
    /* The desktop's delete does exist on the same render, so the absence above is this surface's decision. */
    expect(container.querySelectorAll('[data-nc-desktop-panel] [data-nc-row-action]').length)
      .toBeGreaterThan(0);
  });
});

describe('the rendered mobile Tasks page projects its view model faithfully', () => {
  it('across ready, dispatched, withdrawn and unreadable rows', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    expect(checkProjectionIn(painter(), tasksModule(TASKS), mobilePanel(container))).toEqual([]);
  });

  it('with zero tasks', () => {
    const { container } = renderPage({ tasks: [], panel: 'tasks' });
    expect(checkProjectionIn(painter(), tasksModule([]), mobilePanel(container))).toEqual([]);
  });

  it('is not vacuous: the marked rows are in the mobile subtree', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    const root = mobilePanel(container);
    expect(MARKER.row).toBe('data-nc-row');
    expect(MARKER.module).toBe('data-nc-module');
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(TASKS.length);
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
  });

  it('hosts reveal-block on the row root, which also carries the row marker', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    const root = mobilePanel(container);
    expect(MARKER.action).toBe('data-nc-row-action');
    const rows = Array.from(root.querySelectorAll('[data-nc-row]'));
    expect(rows.length).toBe(TASKS.length);
    for (const row of rows) {
      expect(row.getAttribute('data-nc-row-action')).toBe('reveal-block');
      expect(row.querySelectorAll('[data-nc-row-action]').length).toBe(0);
    }
    expect(checkProjectionIn(painter(), tasksModule(TASKS), root)).toEqual([]);
  });

  /* `struck` has no code in the checker at all, so it needs a carrier beside the projection. */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    const badges = Array.from(mobilePanel(container).querySelectorAll('[data-nc-badge]'));
    expect(badges.map((badge) => badge.textContent))
      .toEqual(['Unreadable', 'Not ready', 'Withdrawn']);
    expect(badges[2].className).toContain('mobileRowStruck');
    expect(badges[1].className).not.toContain('mobileRowStruck');
    expect(badges[0].className).not.toContain('mobileRowStruck');
  });

  /* Scoped: the undispatched row on the same page keeps its word — the rule is "a run supersedes the word", not "the word is gone". */
  it('shows the run instead of the readiness word it used to invent', () => {
    const { container } = renderPage({ tasks: TASKS.slice(0, 2), panel: 'tasks' });
    const text = mobilePanel(container).textContent ?? '';
    expect(text).not.toContain('Ready');
    expect(text).not.toContain('Not ready');
    expect(text).toContain('failed');
  });
});

/* The projection checker does not read `PanelRow.activity`, so the indicator is held by a same-set comparison over the two panel subtrees; the page head is excluded because the mobile head paints no indicator. */
describe('mobile and desktop paint the same data-nc-activity set', () => {
  const VOCABULARY = /^(Working|Needs input|Needs attention|Unread updates)$/;
  const activityByRow = (root: Element): Record<string, [string | null, string | null]> => Object.fromEntries(
    [...root.querySelectorAll('[data-nc-row]')].map((row): [string, [string | null, string | null]] => {
      const state = row.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null;
      const spoken = within(row as HTMLElement).queryByText(VOCABULARY)?.textContent ?? null;
      return [row.getAttribute('data-nc-row') ?? '', [state, spoken]];
    }),
  );
  const verdicts = track({ cards: { 'card-1': 'working', 'card-9': 'failed' } });

  it.each(['cards', 'tasks'] as const)('on the %s module', (panel) => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, track: verdicts, panel });
    const mobile = activityByRow(mobilePanel(container));
    const desktop = activityByRow(container.querySelector('[data-nc-desktop-panel]')!);
    const module = new Set((panel === 'cards' ? cardsModule(CARDS) : tasksModule(TASKS))
      .flatMap((view) => view.rows.map((row) => row.id)));
    const desktopModule = Object.fromEntries(Object.entries(desktop).filter(([id]) => module.has(id)));
    /* Non-vacuous: the fixture reaches a working card, a failed worker-card
       task, and rows with no verdict at all. */
    expect(Object.values(desktopModule).map(([state]) => state)).toContain(panel === 'cards' ? 'working' : 'failed');
    expect(Object.values(desktopModule).map(([state]) => state)).toContain(null);
    /* Each side speaks exactly its own verdict, in the one vocabulary. */
    for (const [state, spoken] of [...Object.values(desktopModule), ...Object.values(mobile)]) {
      expect(spoken).toBe(state === null ? null : activityLabelOf(state as ActivityState));
    }
    expect(mobile).toEqual(desktopModule);
  });
});

describe('wording hygiene guard: the page words no task state of its own', () => {
  /* A hygiene guard, not the oracle: `Withdrawn` and `Unreadable` are included even though the derivation produces them — the fault is the page deciding the word. */
  const STATE_WORDS: readonly string[] = ['Ready', 'Not ready', 'Withdrawn', 'Unreadable'];

  it('spells none of the four declaration words', () => {
    for (const word of STATE_WORDS) {
      expect(pageSource, `public.tsx must not spell ${word}`).not.toContain(word);
    }
  });

  it('is scanning this page’s source', () => {
    expect(pageSource).toContain('export function TrackPage');
    expect(pageSource).toContain('paintMobileModule');
  });
});
