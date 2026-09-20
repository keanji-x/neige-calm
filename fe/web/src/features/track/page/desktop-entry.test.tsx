// @vitest-environment jsdom
//
// The entry oracle: the page renders the painter's whole return value and draws no marker-bearing panel beside it.
// A whole file for it: `vi.mock` is module-wide and must not sit under the projection suite.

import { cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { RowPainter, TrackPageView } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import type { DesktopLeaf } from './desktop-painter.tsx';
import { card, openableCardsOf, renderPage } from './test-fixtures.tsx';

/** Every call the page made, in order. */
const calls: { painter: RowPainter<DesktopLeaf>; view: TrackPageView }[] = [];

/** What the mock hands back: `wrap` — the painter's own modules with the tag in front; `replace` — the tag and nothing else. */
let mode: 'wrap' | 'replace' = 'wrap';

vi.mock('./desktop-painter.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./desktop-painter.tsx')>();
  return {
    ...actual,
    paintDesktopPanel: (painter: RowPainter<DesktopLeaf>, view: TrackPageView): readonly ReactNode[] => {
      calls.push({ painter, view });
      /* The tag is not a projection marker and deliberately shares no prefix with one. */
      const tag = <div key="painted-here" data-entry-oracle-tag="" />;
      return mode === 'replace' ? [tag] : [tag, ...actual.paintDesktopPanel(painter, view)];
    },
  };
});

afterEach(() => {
  cleanup();
  calls.length = 0;
  mode = 'wrap';
});

const CARDS: readonly CardWire[] = [
  card({ id: 'card-1', kind: 'terminal', title: 'Build log', deletable: true }),
  card({ id: 'card-2', kind: 'harness', title: null, deletable: false }),
];

const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'block-1', key: 'alpha-gate', state: 'ready', declaration: null,
    status: 'running', statusDetail: 'step 2 of 3', kind: 'codex', workerCardId: 'card-1', pendingReason: null,
  },
  {
    blockId: 'block-2', key: 'beta-gate', state: 'withdrawn', declaration: 'Withdrawn',
    status: null, statusDetail: null, kind: null, workerCardId: null, pendingReason: null,
  },
];

/** Strings only the painted row modules can put on screen; read as substrings of the subtree's `textContent`. */
const PAINTED_TEXT: readonly string[] = [
  'Cards', 'Tasks', 'Build log', 'harness', 'alpha-gate', 'beta-gate',
];

/** The desktop panel subtree: the mobile surface is a sibling that is in the DOM at the same time. */
function desktopPanel(container: Element): Element {
  const root = container.querySelector('[data-nc-desktop-panel]');
  expect(root, 'the desktop panel surface must be findable').not.toBeNull();
  return root!;
}

describe('the page paints its desktop panel through paintDesktopPanel', () => {
  it('calls it once, with the whole derived view and both modules', () => {
    renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });

    expect(calls.length, 'paintDesktopPanel calls').toBe(1);
    expect(calls[0].view).toEqual(deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) }));
    expect(calls[0].view.rowModules.map((module) => module.key)).toEqual(['cards', 'tasks']);
    expect(calls[0].view.rowModules.map((module) => module.rows.length))
      .toEqual([CARDS.length, TASKS.length]);
  });

  it('renders what it handed back, inside the desktop panel', () => {
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(2);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length + TASKS.length);
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  it('and draws no Cards/Tasks row-module of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({ cards: CARDS, tasks: TASKS, onDeleteCard: vi.fn() });
    const root = desktopPanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-module]').length, 'row modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
  });
});
