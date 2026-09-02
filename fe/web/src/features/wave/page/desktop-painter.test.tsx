// @vitest-environment jsdom
//
// #1234 S1b-3b — the desktop painter, against `checkProjection`'s synthetic
// mount.
//
// **This file checks the painter, not the page.** It paints with the painter it
// checks, which is the one thing `checkProjection` guarantees and
// `checkProjectionIn` cannot. What it says nothing about is whether
// `public.tsx` renders through this painter at all — that is
// `desktop-projection.test.tsx`, plus the structural constraint that the page
// may spell no marker literal of its own.
//
// The capability cases are here rather than there because support is a *painter*
// fact: `delete-card` is supported when and only when the host passed
// `onDeleteCard`, and that binding is what stops the desktop growing a delete
// control on a page that offers no deletion (Δ3).

import { render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { PanelRow, RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode } from '../../../../../tools/projection/public.ts';
import { makeDesktopPainter, type DesktopLeaf } from './desktop-painter.tsx';

/** The one mount every case here uses: it resolves each module leaf and renders
 *  exactly what the painter painted, nothing fabricated. */
const mount = (painted: readonly DesktopLeaf[]): ProjectionNode =>
  render(<>{painted.map((leaf) => leaf.paint('cards'))}</>).container;

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

/** No title, so the kind took the name slot and there is no separate `kind`
 *  field; kernel-owned, so a badge stands where the × would be and the view
 *  model derives no `delete-card` at all. */
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
