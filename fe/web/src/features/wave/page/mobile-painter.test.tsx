// @vitest-environment jsdom
//
// #1234 S1b-4a — the mobile painter, against `checkProjection`'s synthetic
// mount.
//
// **This file checks the painter, not the page.** It paints with the painter it
// checks, which is the one thing `checkProjection` guarantees and
// `checkProjectionIn` cannot. Whether `public.tsx` renders through this painter
// at all is `mobile-entry.test.tsx`'s claim, with `mobile-projection.test.tsx`
// checking the resulting real DOM against the view model.
//
// The capability cases are here because support is a *painter* fact, and on this
// surface it is the deliberate inconsistency itself (D1 / D7): the two card
// actions are not offered, and each says why.

import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode } from '../../../../../tools/projection/public.ts';
import { makeMobilePainter, paintMobileModule, type MobileLeaf } from './mobile-painter.tsx';

/** The one mount every case here uses: it unwraps each module leaf and renders
 *  exactly what the painter painted, nothing fabricated. */
const mount = (painted: readonly MobileLeaf[]): ProjectionNode =>
  render(<>{painted.map((leaf) => {
    if (leaf.slot !== 'module') throw new Error(`checkProjection handed back a ${leaf.slot} leaf`);
    return leaf.node;
  })}</>).container;

/** A titled, deletable card: `title !== kind`, so both fields have a carrier of
 *  their own, and the derivation offers the two actions this surface refuses. */
const titled: PanelRow = {
  id: 'card-1',
  title: 'Build log',
  kind: 'terminal',
  badges: [],
  status: null,
  actions: [
    { kind: 'open-card', cardId: 'card-1', label: null, hint: null },
    { kind: 'delete-card', cardId: 'card-1', label: 'Delete card Build log', hint: 'Delete card' },
  ],
};

/** Untitled and kernel-owned: the derived name *is* the kind, so there is no
 *  separate `kind` carrier — the row that used to print the word twice. */
const untitled: PanelRow = {
  id: 'card-2',
  title: 'harness',
  kind: null,
  badges: [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
  status: null,
  actions: [{ kind: 'open-card', cardId: 'card-2', label: null, hint: null }],
};

const cardsModule: RowModuleView = {
  key: 'cards', title: 'Cards', empty: 'No cards yet.', rows: [titled, untitled],
};
const emptyCards: RowModuleView = { ...cardsModule, rows: [] };

const painter = () => makeMobilePainter({ backLabel: 'Report', onBack: vi.fn() });

describe('the mobile painter’s capability table', () => {
  it('supports reveal-block', () => {
    expect(painter().action['reveal-block']).toEqual({ supported: true });
  });

  it('offers neither card action, and says why for each', () => {
    for (const kind of ['open-card', 'delete-card'] as const) {
      const support = painter().action[kind];
      expect(support.supported, kind).toBe(false);
      expect(support.supported === false && support.why.length > 0, kind).toBe(true);
    }
  });

  /* The table is not bound to a host prop on this surface, unlike the desktop's
     `delete-card`: not offering the card actions is a fact about the viewport. */
  it('says the same thing whatever the host passes', () => {
    const withHandlers = makeMobilePainter({ onOpenTask: vi.fn(), backLabel: 'Report', onBack: vi.fn() });
    expect(withHandlers.action).toEqual(makeMobilePainter({}).action);
  });
});

describe('the mobile painter is a faithful projection of a Cards module', () => {
  it('with a titled row and an untitled kernel-owned one', () => {
    expect(checkProjection(painter(), [cardsModule], mount)).toEqual([]);
  });

  it('with zero rows', () => {
    expect(checkProjection(painter(), [emptyCards], mount)).toEqual([]);
  });
});

describe('what the painted Cards module puts on screen', () => {
  const paint = (module: RowModuleView) =>
    render(<>{paintMobileModule(painter(), module)}</>).container;

  /* The row is not a control: both of its actions were filtered away, so no
     `onSelect` was passed and Astryx generated no invisible button. The whole
     mobile Cards list therefore contains exactly one button — the header's
     Back. */
  it('renders no per-row control', () => {
    const container = paint(cardsModule);
    const buttons = Array.from(container.querySelectorAll('button'));
    expect(buttons.length).toBe(1);
    expect(buttons[0].getAttribute('aria-label')).toBe('Back to Report');
  });

  /* The regression this slice exists to remove: an untitled card's name *is*
     its kind, and the old mobile list printed it a second time in the meta
     lane. */
  it('prints an untitled card’s kind once, not twice', () => {
    const container = paint({ ...cardsModule, rows: [untitled] });
    const occurrences = (container.textContent ?? '').split('harness').length - 1;
    expect(occurrences).toBe(1);
  });

  it('prints a titled card’s name and its kind, and the ownership badge', () => {
    const container = paint(cardsModule);
    expect(container.textContent).toContain('Build log');
    expect(container.textContent).toContain('terminal');
    expect(container.textContent).toContain('kernel-owned');
  });

  /* A Task row's composition is not this file's yet, and painting a Cards row
     for it would put the fault far from its cause. */
  it('refuses a Tasks module rather than guessing at its rows', () => {
    expect(() => paint({ ...cardsModule, key: 'tasks', title: 'Tasks' }))
      .toThrow(/tasks row/);
  });
});
