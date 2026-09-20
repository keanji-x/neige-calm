// @vitest-environment jsdom
//
// The mobile entry oracle, over both row modules: the page renders the painter's return value and draws no list of its own beside it.
// A whole file for it: `vi.mock` is module-wide and must not sit under the projection suite.

import { cleanup } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire } from '../../../../../core/domain/track.ts';
import { MARKER } from '../../../../../core/view/panel.ts';
import type { RowModuleView, RowPainter } from '../../../../../core/view/panel.ts';
import { deriveTrackPageView } from '../../../../../core/view/track-page.ts';
import type { MobileLeaf } from './mobile-painter.tsx';
import { card, openableCardsOf, renderPage } from './test-fixtures.tsx';

/** Every call the page made, in order. */
const calls: { painter: RowPainter<MobileLeaf>; module: RowModuleView }[] = [];

/** What the mock hands back: `wrap` — the painted module with the tag in front; `replace` — the tag and nothing else. */
let mode: 'wrap' | 'replace' = 'wrap';

vi.mock('./mobile-painter.tsx', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./mobile-painter.tsx')>();
  return {
    ...actual,
    paintMobileModule: (painter: RowPainter<MobileLeaf>, module: RowModuleView): ReactNode => {
      calls.push({ painter, module });
      /* The tag is not a projection marker and deliberately shares no prefix with one. */
      const tag = <div key="painted-here" data-entry-oracle-tag="" />;
      return mode === 'replace'
        ? tag
        : <>{tag}{actual.paintMobileModule(painter, module)}</>;
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

/** Strings only the painted Cards module can put on this surface; read as substrings of the subtree's `textContent`. */
const PAINTED_TEXT: readonly string[] = ['Cards', 'Build log', 'harness'];

/** `beta-gate` carries a run, so its row prints a word only the painter can produce here. */
const TASKS: readonly ReportTaskRow[] = [
  {
    blockId: 'block-1', key: 'alpha-impl', state: 'ready', declaration: null,
    status: null, statusDetail: null, kind: 'codex', workerCardId: null, pendingReason: null,
  },
  {
    blockId: 'block-2', key: 'beta-gate', state: 'not-ready', declaration: null,
    status: 'failed', statusDetail: 'not a git repository',
    kind: 'terminal', workerCardId: 'card-9', pendingReason: null,
  },
];

/** The module title, both task keys, and the status word. */
const PAINTED_TASK_TEXT: readonly string[] = ['Tasks', 'alpha-impl', 'beta-gate', 'failed'];

/** The mobile panel subtree: the desktop surface is a sibling in the same DOM and carries markers of its own. */
function mobilePanel(container: Element): Element {
  const root = container.querySelector('[data-nc-mobile-panel]');
  expect(root, 'the mobile panel surface must be findable').not.toBeNull();
  return root!;
}

describe('the page paints its mobile Cards page through paintMobileModule', () => {
  it('calls it once, with the Cards module of the derived view', () => {
    renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });

    expect(calls.length, 'paintMobileModule calls').toBe(1);
    const expected = deriveTrackPageView({ cards: CARDS, tasks: [], activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, []) }).rowModules
      .find((module) => module.key === 'cards');
    expect(calls[0].module).toEqual(expected);
    expect(calls[0].module.key).toBe('cards');
    expect(calls[0].module.rows.map((row) => row.id)).toEqual(['card-1', 'card-2']);
  });

  it('does not call it while the panel is closed', () => {
    renderPage({ cards: CARDS });
    expect(calls.length).toBe(0);
  });

  it('renders what it handed back, inside the mobile panel', () => {
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(CARDS.length);
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  it('and draws no Cards list of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({ cards: CARDS, panel: 'cards', onDeleteCard: vi.fn() });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-module]').length, 'modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    for (const text of PAINTED_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
  });
});

describe('the page paints its mobile Tasks page through paintMobileModule', () => {
  it('calls it once, with the Tasks module of the derived view', () => {
    renderPage({ cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn() });

    expect(calls.length, 'paintMobileModule calls').toBe(1);
    const expected = deriveTrackPageView({ cards: CARDS, tasks: TASKS, activity: NEUTRAL_ACTIVITY, openableCards: openableCardsOf(CARDS, TASKS) }).rowModules
      .find((module) => module.key === 'tasks');
    expect(calls[0].module).toEqual(expected);
    expect(calls[0].module.key).toBe('tasks');
    expect(calls[0].module.rows.map((row) => row.id)).toEqual(['block-1', 'block-2']);
  });

  it('renders what it handed back, inside the mobile panel', () => {
    const { container } = renderPage({
      cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn(),
    });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length, 'the painter’s node').toBe(1);
    expect(MARKER.module).toBe('data-nc-module');
    expect(MARKER.row).toBe('data-nc-row');
    expect(root.querySelectorAll('[data-nc-module]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-row]').length).toBe(TASKS.length);
    for (const text of PAINTED_TASK_TEXT) {
      expect(root.textContent, `wrap renders ${text}`).toContain(text);
    }
  });

  it('and draws no Tasks list of its own beside it, for this fixture', () => {
    mode = 'replace';
    const { container } = renderPage({
      cards: CARDS, tasks: TASKS, panel: 'tasks', onOpenTask: vi.fn(),
    });
    const root = mobilePanel(container);

    expect(root.querySelectorAll('[data-entry-oracle-tag]').length).toBe(1);
    expect(root.querySelectorAll('[data-nc-module]').length, 'modules the page drew itself').toBe(0);
    expect(root.querySelectorAll('[data-nc-row]').length, 'rows the page drew itself').toBe(0);
    for (const text of PAINTED_TASK_TEXT) {
      expect(root.textContent, `no unmarked copy of ${text} survives`).not.toContain(text);
    }
    for (const word of ['Ready', 'Not ready', 'Withdrawn', 'Unreadable']) {
      expect(root.textContent, `no ${word} survives`).not.toContain(word);
    }
  });
});
