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
// nothing here can see it. The gap is real and is S1b-4b's (`rowModules`-derived
// menu entries); it is recorded here rather than papered over.
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

import type { CardWire } from '../../../../../core/domain/wave.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { PanelRow } from '../../../../../core/view/panel.ts';
import { deriveWavePageView } from '../../../../../core/view/wave-page.ts';
import { checkProjectionIn } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter } from './mobile-painter.tsx';
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
   * half of the action layer have no reachable case here. They stay covered by
   * `projection-contract.test.tsx`'s synthetic painters and, once the mobile
   * Tasks page exists, by S1b-4b.
   */
  it('carries no status at all, which is why the status clauses are absent above', () => {
    expect(ROWS.every((row) => row.status === null)).toBe(true);
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
   * The three visible consequences of the capability table, asserted on the real
   * page: no delete control, no row control, and — because the row is not a
   * button — the accessible name of the Cards list is carried by visible text.
   * The projection cannot see any of this (§6.3 declines interactivity), which
   * is why it is a behaviour assertion beside it rather than a code in the
   * checker.
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
