// @vitest-environment jsdom
//
// #1234 S1b-4a — the **real mobile page** as a faithful projection of its view
// model.
//
// `mobile-painter.test.tsx` checks the painter in isolation, which is exactly
// the shape §6.10 names: a green synthetic painter says nothing about whether
// the page renders through it, and until this slice the mobile surface did not.
// So this suite renders the unmodified `WavePage` with `panel="cards"`, takes
// the **mobile panel subtree**, and runs `checkProjectionIn` over it.
//
// **Why the subtree and not the container.** `.mobileListSurface` and
// `.desktopPanelSurface` are siblings and both in the DOM in production — only
// `inert` and `aria-hidden` distinguish them. Until this slice a whole-page scan
// happened to work because the mobile side carried no markers; now both sides do,
// and an unscoped scan would read the two as one tree with four modules in it.
// Both projection suites scope, and for the same reason.
//
// **One module, not two.** `checkProjectionIn`'s `modules` argument is the
// sequence this tree must hold — the desktop passes both row modules, mobile
// passes the one the reader drilled into (Δ2). That is not a weaker check of the
// same thing: on this surface the module *sequence* is the navigation menu, and
// nothing here can see it. Since S1b-4b that sequence has its own carrier —
// `public.test.tsx`'s "the drill-down menu offers exactly the derived row
// modules", which compares the menu against `rowModules` and follows each entry
// into the page it opens.
//
// **Since S1b-4b this file covers both mobile row modules.** The Tasks fixture
// below reaches the clauses a Cards row cannot: a status (both null and not),
// a phrase wider than its token, `struck` both ways, and an action layer that is
// non-vacuous in *both* directions — `reveal-block` supported and painted,
// `open-card` offered by the derivation and filtered away.
//
// **What this suite is worth, and what carries the rest.** `checkProjectionIn`
// takes the painter on trust: it cannot prove this DOM came from that painter,
// and the painter built here is a second construction, not the page's. What
// turns a green run into evidence is **`mobile-entry.test.tsx`**, which mocks
// `paintMobileModule` and holds that the page calls it with the Cards module and
// renders the value it hands back.
//
// **The marker-literal hygiene guard is not repeated here.** It already covers
// this branch: `desktop-projection.test.tsx`'s last describe scans the *whole* of
// `public.tsx` — both surfaces — for every `MARKER` name in both its kebab and
// its `dataset` spelling, so a hand-composed mobile row that retyped
// `data-nc-row` goes red there. Read it as hygiene either way; the oracle is the
// entry suite.

import { cleanup, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import type { CardWire } from '../../../../../core/domain/wave.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { deriveWavePageView } from '../../../../../core/view/wave-page.ts';
import { checkProjectionIn } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter } from './mobile-painter.tsx';
import pageSource from './public.tsx?raw';
import { card, renderPage } from './test-fixtures.tsx';

afterEach(cleanup);

/*
 * The fixture, shaped to §3.5's oracle preconditions as far as a Cards module
 * can carry them. The clauses that this module cannot reach at all — status,
 * multiple badges, a supported action — are named in the shape guard rather than
 * quietly dropped.
 */
const CARDS: readonly CardWire[] = [
  /* Titled: `title !== kind`, so both fields have a carrier of their own, and
     with `onDeleteCard` passed the derivation really does offer a `delete-card`
     for the capability table to filter away. */
  card({ id: 'card-1', kind: 'terminal', title: 'Build log', deletable: true }),
  /* Untitled and kernel-owned: `kind === null`, one badge. The row that used to
     print `harness` twice on this surface. */
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

/** The painter the page builds for this render, rebuilt here. The duplication is
 *  the trust boundary named in the file head; `mobile-entry.test.tsx` closes it
 *  by holding the call, not the marker spelling. */
const painter = () => makeMobilePainter({ onOpenTask: vi.fn(), backLabel: 'Report', onBack: vi.fn() });

/** The mobile panel subtree — not the container. */
function mobilePanel(container: Element): Element {
  const root = container.querySelector('[data-nc-mobile-panel]');
  expect(root, 'the mobile panel surface must be findable').not.toBeNull();
  return root!;
}

const cardsModule = (cards: readonly CardWire[]) =>
  deriveWavePageView({ cards, tasks: [] }).rowModules.filter((module) => module.key === 'cards');

/*
 * The Tasks fixture (S1b-4b), written as `deriveReportTasks` produces its rows
 * — `app/router` joins declarations with kernel verdicts and hands this shape
 * over. The six clauses §3.5 asks for are each carried by one row below, and the
 * shape guard re-reads them off the *derived* rows so a fixture edited into
 * uniformity cannot make the projection vacuous.
 */
const TASKS: readonly ReportTaskRow[] = [
  /* Ready and undispatched: no declaration word (D8 — `Ready` is gone from this
     surface because `declarationWord` never produced it), no run, and a kind
     with no worker card, so the derivation offers `reveal-block` alone. */
  {
    blockId: 'block-1', key: 'alpha-impl', state: 'ready', declaration: null,
    status: null, statusDetail: null, kind: 'codex', workerCardId: null,
  },
  /* Dispatched, with a reason and a worker card: the status swallowed the
     readiness word (D8 — `Not ready` no longer appears once there is a run),
     `phrase` is `status — detail`, and `kind !== null && workerCardId !== null`
     makes the derivation offer the `open-card` this surface refuses. */
  {
    blockId: 'block-2', key: 'beta-gate', state: 'not-ready', declaration: null,
    status: 'failed', statusDetail: 'wave /tmp/alpha is not a git repository',
    kind: 'terminal', workerCardId: 'card-9',
  },
  /* Withdrawn: a struck declaration, and `kind === null` — upstream makes the
     kind and the card id null together for exactly these rows. */
  {
    blockId: 'block-3', key: 'gamma-spec', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null,
  },
  /* Unreadable: an ordinary, unstruck declaration beside the withdrawn one, so
     the struck assertion below has both directions. */
  {
    blockId: 'block-4', key: 'delta-doc', state: 'unreadable', declaration: 'Unreadable',
    status: null, statusDetail: null, kind: null, workerCardId: null,
  },
];

const tasksModule = (tasks: readonly ReportTaskRow[]): readonly RowModuleView[] =>
  deriveWavePageView({ cards: [], tasks }).rowModules.filter((module) => module.key === 'tasks');

// ── The fixture shape guard ──────────────────────────────────────────────────

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

  /*
   * **Both card actions are exercised, and both are filtered away.** Without
   * this the action-layer check would be vacuous here in the worst way: a
   * painter that grew a control would be caught, but a fixture that never
   * offered an action in the first place proves nothing about the capability
   * table at all.
   */
  it('the derivation offers both unsupported actions, so filtering them is not vacuous', () => {
    const offered = ROWS.flatMap((row) => row.actions.map((action) => action.kind));
    expect(offered).toContain('open-card');
    expect(offered).toContain('delete-card');
    for (const kind of offered) expect(painter().action[kind].supported).toBe(false);
  });

  /*
   * **Recorded, not dropped.** A Cards row never carries a status, never more
   * than one badge, and never an action this surface supports — so `status`,
   * `phrase !== token`, the empty token, badge multiplicity and the supported
   * half of the action layer have no reachable case here. The first four are
   * the Tasks fixture's job below; badge multiplicity is reachable on neither
   * module and stays with `projection-contract.test.tsx`'s synthetic painters.
   */
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

  /* The status clauses the Cards module could not reach: a row with a run and a
     row without, and a `phrase` that is strictly more than its `token` — which
     is what makes `status-token` and `status-phrase` two separable faults
     rather than one. */
  it('exercises status both null and non-null, with a phrase wider than its token', () => {
    expect(TASK_ROWS.some((row) => row.status === null)).toBe(true);
    const withStatus = TASK_ROWS.filter((row) => row.status !== null);
    expect(withStatus.length).toBeGreaterThan(0);
    for (const row of withStatus) {
      expect(row.status!.phrase).not.toEqual(row.status!.token);
      expect(row.status!.phrase.startsWith(row.status!.token)).toBe(true);
    }
  });

  /* `struck` both ways, and both declarations present: the badge layer here is
     one badge or none, so the pair below is the whole reachable range. */
  it('exercises a struck declaration, an unstruck one, and a row with none', () => {
    const badges = TASK_ROWS.flatMap((row) => [...row.badges]);
    expect(badges.some((badge) => badge.struck)).toBe(true);
    expect(badges.some((badge) => !badge.struck)).toBe(true);
    expect(TASK_ROWS.some((row) => row.badges.length === 0)).toBe(true);
  });

  /*
   * **The action layer is non-vacuous in both directions here**, which the Cards
   * module could not be: `reveal-block` is supported and must be painted on
   * every row, and `open-card` is offered by the derivation on the worker-card
   * row and must be filtered away. A fixture that offered only one of the two
   * would leave half the layer proving nothing.
   */
  it('offers a supported action on every row and an unsupported one on some', () => {
    for (const row of TASK_ROWS) {
      expect(row.actions.map((action) => action.kind)).toContain('reveal-block');
    }
    const offered = TASK_ROWS.flatMap((row) => row.actions.map((action) => action.kind));
    expect(offered).toContain('open-card');
    expect(painter().action['reveal-block'].supported).toBe(true);
    expect(painter().action['open-card'].supported).toBe(false);
  });

  /* D8, read off the derivation rather than off the DOM: neither word this
     surface used to write for itself survives the trip. */
  it('produces neither Ready nor Not ready for these rows', () => {
    const words = TASK_ROWS.flatMap((row) => row.badges.map((badge) => badge.text));
    expect(words).not.toContain('Ready');
    expect(words).not.toContain('Not ready');
  });
});

// ── The projection itself ────────────────────────────────────────────────────

describe('the rendered mobile Cards page projects its view model faithfully', () => {
  it('with a titled row and an untitled kernel-owned one', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    expect(checkProjectionIn(painter(), cardsModule(CARDS), mobilePanel(container))).toEqual([]);
  });

  /* The capability table is checked against a host that *does* offer deletion:
     the desktop grows an × from this same prop, and the mobile page must not. */
  it('with no delete handler either', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards' });
    expect(checkProjectionIn(painter(), cardsModule(CARDS), mobilePanel(container))).toEqual([]);
  });

  it('with zero cards', () => {
    const { container } = renderPage({ cards: [], panel: 'cards' });
    expect(checkProjectionIn(painter(), cardsModule([]), mobilePanel(container))).toEqual([]);
  });

  /* The rows really are on the page these assertions inspect — a projection over
     an empty subtree is green for the wrong reason. */
  it('is not vacuous: the marked rows are in the mobile subtree', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);
    /* Static selectors — `no-class-dom-query` requires them — pinned to `MARKER`
       on the spot so the two literals cannot drift from the table. */
    expect(MARKER.row).toBe('data-nc-row');
    expect(MARKER.module).toBe('data-nc-module');
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length);
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
  });

  /*
   * The two visible consequences of the capability table, asserted on the real
   * page: no delete control and no row control — so, the row being no button,
   * it remains represented by visible text (which the projection checks above)
   * rather than by a control's name. Nothing here computes an accessible name;
   * the assertions below are button counts. The projection cannot see
   * interactivity at all (§6.3 declines it), which is why this is a behaviour
   * assertion beside it rather than a code in the checker.
   */
  it('offers no card affordance: no delete, and the row is not a control', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);
    expect(root.querySelectorAll('[data-nc-row] button').length).toBe(0);
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
    /* The desktop's delete does exist on the same render — so the absence above
       is this surface's decision, not the fixture withholding the handler. */
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

  /*
   * **The co-hosted shape, asserted rather than implied.** S1b-2 wrote
   * `owned()`'s self-inclusion clause for the mobile row that is its own action
   * host, and nothing had taken that path in production until this slice. The
   * green run above is the projection's verdict on it; this is the shape itself,
   * so a painter that moved the action marker onto a child would be visible as a
   * changed *shape* and not only as a changed result.
   */
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

  /* `struck` has no code in the checker at all, so it needs a carrier beside the
     projection — the same shape the desktop's `taskWithdrawn` assertion takes,
     and both directions so an unconditional class cannot pass. */
  it('strikes through a withdrawn declaration but not an ordinary one', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    const badges = Array.from(mobilePanel(container).querySelectorAll('[data-nc-badge]'));
    expect(badges.map((badge) => badge.textContent)).toEqual(['Withdrawn', 'Unreadable']);
    expect(badges[0].className).toContain('mobileRowStruck');
    expect(badges[1].className).not.toContain('mobileRowStruck');
  });

  /* D8 on the real page: the two words this branch used to write for itself are
     gone, and what replaced the second one is the run. */
  it('shows the run instead of the readiness word it used to invent', () => {
    const { container } = renderPage({ tasks: TASKS, panel: 'tasks' });
    const text = mobilePanel(container).textContent ?? '';
    expect(text).not.toContain('Ready');
    expect(text).not.toContain('Not ready');
    expect(text).toContain('failed');
  });
});

// ── A hygiene guard beside the above, not a premise of it ────────────────────

describe('wording hygiene guard: the page words no task state of its own', () => {
  /*
   * The mobile Tasks branch used to hold the four words `deriveReportTasks`
   * already produces, re-derived from `task.state` — which is how this surface
   * came to say `Ready` where the desktop said nothing and `Not ready` where the
   * desktop showed the run. The branch is gone (S1b-4b), and this stops the
   * cheapest way of putting it back: writing one of those words in this file
   * again.
   *
   * **A hygiene guard, not the oracle.** A page can still put a word on screen
   * without spelling it — by a computed string, or through a component that
   * carries wording of its own — so what actually holds the branch is
   * `mobile-entry.test.tsx`'s `replace` case (nothing of the Tasks list survives
   * the painter's output being taken away) and the projection above. This is the
   * narrow, mechanical companion to those, in the shape the marker-literal scan
   * in `desktop-projection.test.tsx` already takes for attribute names.
   *
   * **`Withdrawn` and `Unreadable` are here for the same reason as the other
   * two**, even though the derivation still produces them: the fault is the page
   * *deciding* the word, not the word being wrong. A branch that re-derived
   * `Withdrawn` correctly today is the identical structure that got `Ready`
   * wrong.
   */
  const STATE_WORDS: readonly string[] = ['Ready', 'Not ready', 'Withdrawn', 'Unreadable'];

  it('spells none of the four declaration words', () => {
    for (const word of STATE_WORDS) {
      expect(pageSource, `public.tsx must not spell ${word}`).not.toContain(word);
    }
  });

  /* The scan is over real source: a mis-wired import would make the assertion
     above vacuously true. */
  it('is scanning this page’s source', () => {
    expect(pageSource).toContain('export function WavePage');
    expect(pageSource).toContain('paintMobileModule');
  });
});
