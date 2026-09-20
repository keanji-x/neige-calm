// @vitest-environment jsdom
//
// The desktop painter against `checkProjection`'s synthetic mount: this file checks the painter, not the page.

import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode } from '../../../../../tools/projection/public.ts';
import { makeDesktopPainter, type DesktopLeaf } from './desktop-painter.tsx';

/** The one mount every case uses: it renders exactly what the painter painted; a non-module leaf at this level is a thrown error, not a guess. */
const mount = (painted: readonly DesktopLeaf[]): ProjectionNode =>
  render(<>{painted.map((leaf) => {
    if (leaf.slot !== 'module') throw new Error(`checkProjection handed back a ${leaf.slot} leaf`);
    return leaf.node;
  })}</>).container;

const titled: PanelRow = {
  id: 'card-1',
  title: 'Build log',
  kind: 'terminal',
  badges: [],
  status: null,
  activity: null,
  actions: [
    { kind: 'open-card', cardId: 'card-1', label: null, hint: null, description: null },
    {
      kind: 'delete-card', cardId: 'card-1', label: 'Delete card Build log',
      hint: 'Delete card', description: null,
    },
  ],
};

/** No title, so the kind took the name slot; kernel-owned, so the view model derives no `delete-card`. */
const untitled: PanelRow = {
  id: 'card-2',
  title: 'harness',
  kind: null,
  badges: [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
  status: null,
  activity: null,
  actions: [{ kind: 'open-card', cardId: 'card-2', label: null, hint: null, description: null }],
};

const cardsModule: RowModuleView = {
  key: 'cards', title: 'Cards', empty: 'No cards yet.', rows: [titled, untitled],
};
const emptyCards: RowModuleView = { ...cardsModule, rows: [] };

describe('the desktop painter’s capability table', () => {
  it('supports delete-card when the host passed onDeleteCard', () => {
    expect(makeDesktopPainter({ onDeleteCard: vi.fn() }).action['delete-card'])
      .toEqual({ supported: true });
  });

  it('does not support delete-card when the host passed none, and says why', () => {
    const support = makeDesktopPainter({}).action['delete-card'];
    expect(support.supported).toBe(false);
    expect(support.supported === false && support.why).toContain('onDeleteCard');
  });

  it('supports the two navigations unconditionally — the page draws them with or without a callback', () => {
    const painter = makeDesktopPainter({});
    expect(painter.action['open-card']).toEqual({ supported: true });
    expect(painter.action['reveal-block']).toEqual({ supported: true });
  });
});

describe('the desktop painter is a faithful projection of a Cards module', () => {
  it('with a titled deletable row and an untitled kernel-owned one', () => {
    expect(checkProjection(makeDesktopPainter({ onDeleteCard: vi.fn() }), [cardsModule], mount))
      .toEqual([]);
  });

  it('with no delete handler, so every delete-card is filtered away', () => {
    expect(checkProjection(makeDesktopPainter({}), [cardsModule], mount)).toEqual([]);
  });

  it('with zero rows', () => {
    expect(checkProjection(makeDesktopPainter({}), [emptyCards], mount)).toEqual([]);
  });
});
