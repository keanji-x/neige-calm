import { describe, expect, it } from 'vitest';

import {
  FIELD,
  MARKER,
  paintModule,
  paintPanel,
  type ActionSupport,
  type PanelRow,
  type RowAction,
  type RowModuleView,
  type RowPainter,
  type TrackPageView,
} from './panel.js';

function row(id: string, title: string, actions: readonly RowAction[] = []): PanelRow {
  return { id, title, kind: null, badges: [], status: null, activity: null, actions };
}

function module(rows: readonly PanelRow[]): RowModuleView {
  return { key: 'cards', title: 'Cards', rows, empty: 'No cards yet.' };
}

const ALL_SUPPORTED: Readonly<Record<RowAction['kind'], ActionSupport>> = {
  'reveal-block': { supported: true },
  'open-card': { supported: true },
  'delete-card': { supported: true },
};

/** Records which leaf constructors ran, and with what. */
function recordingPainter(support: Readonly<Record<RowAction['kind'], ActionSupport>> = ALL_SUPPORTED): {
  painter: RowPainter<string>;
  calls: {
    rows: string[];
    actions: RowAction[][];
    empties: string[];
    modules: { key: string; title: string; children: readonly string[] }[];
  };
} {
  const calls = {
    rows: [] as string[],
    actions: [] as RowAction[][],
    empties: [] as string[],
    modules: [] as { key: string; title: string; children: readonly string[] }[],
  };
  const painter: RowPainter<string> = {
    row: (value) => {
      calls.rows.push(value.id);
      calls.actions.push([...value.actions]);
      return `row:${value.id}`;
    },
    empty: (text) => { calls.empties.push(text); return `empty:${text}`; },
    module: (parts) => { calls.modules.push(parts); return `module:${parts.key}(${parts.children.join(',')})`; },
    action: support,
  };
  return { painter, calls };
}

const REVEAL: RowAction = {
  kind: 'reveal-block', blockId: 'b1', label: null, hint: 'Show b1', description: 'Describe b1',
};
const OPEN: RowAction = { kind: 'open-card', cardId: 'c1', label: null, hint: null, description: null };
const DELETE: RowAction = {
  kind: 'delete-card', cardId: 'c1', label: 'Delete card One', hint: 'Delete card', description: null,
};

describe('paintModule', () => {
  it('paints the empty text, and no rows, for a module with zero rows', () => {
    const { painter, calls } = recordingPainter();
    const painted = paintModule(painter, module([]));

    expect(calls.rows).toEqual([]);
    expect(calls.empties).toEqual(['No cards yet.']);
    expect(painted).toBe('module:cards(empty:No cards yet.)');
  });

  it('paints every row, in order, and never the empty text, for a populated module', () => {
    const { painter, calls } = recordingPainter();
    const painted = paintModule(painter, module([row('a', 'Alpha'), row('b', 'Beta')]));

    expect(calls.rows).toEqual(['a', 'b']);
    expect(calls.empties).toEqual([]);
    expect(painted).toBe('module:cards(row:a,row:b)');
  });

  it('hands the module its key, title and children', () => {
    const { painter, calls } = recordingPainter();
    paintModule(painter, { key: 'tasks', title: 'Tasks', rows: [row('t1', 'One')], empty: 'None.' });

    expect(calls.modules).toEqual([{ key: 'tasks', title: 'Tasks', children: ['row:t1'] }]);
  });
});

describe('paintModule action filtering', () => {
  it('hands `row()` every action, unchanged, when the painter supports them all', () => {
    const { painter, calls } = recordingPainter(ALL_SUPPORTED);
    paintModule(painter, module([row('a', 'Alpha', [REVEAL, OPEN, DELETE])]));

    expect(calls.actions).toEqual([[REVEAL, OPEN, DELETE]]);
  });

  it('withholds an unsupported action from `row()`, keeping the rest in order', () => {
    const { painter, calls } = recordingPainter({
      ...ALL_SUPPORTED,
      'delete-card': { supported: false, why: 'this host passed no delete handler' },
    });
    paintModule(painter, module([row('a', 'Alpha', [REVEAL, OPEN, DELETE])]));

    expect(calls.actions).toEqual([[REVEAL, OPEN]]);
  });

  it('withholds unsupported actions per kind, not per row', () => {
    const { painter, calls } = recordingPainter({
      'reveal-block': { supported: false, why: 'no report on this surface' },
      'open-card': { supported: true },
      'delete-card': { supported: true },
    });
    paintModule(painter, module([
      row('a', 'Alpha', [REVEAL, OPEN]),
      row('b', 'Beta', [REVEAL, DELETE]),
    ]));

    expect(calls.actions).toEqual([[OPEN], [DELETE]]);
  });

  it('hands `row()` no actions when the painter supports none', () => {
    const { painter, calls } = recordingPainter({
      'reveal-block': { supported: false, why: 'no report' },
      'open-card': { supported: false, why: 'no card router' },
      'delete-card': { supported: false, why: 'no delete handler' },
    });
    paintModule(painter, module([row('a', 'Alpha', [REVEAL, OPEN, DELETE])]));

    expect(calls.actions).toEqual([[]]);
  });

  it('leaves every other field of the row alone', () => {
    const seen: PanelRow[] = [];
    const painter: RowPainter<string> = {
      row: (value) => { seen.push(value); return 'row'; },
      empty: () => 'empty',
      module: () => 'module',
      action: { ...ALL_SUPPORTED, 'delete-card': { supported: false, why: 'no handler' } },
    };
    const source: PanelRow = {
      id: 'a',
      title: 'Alpha',
      kind: 'shell',
      badges: [{ id: 'kernel-owned', text: 'kernel-owned', struck: false }],
      status: { token: 'running', phrase: 'running' },
      activity: null,
      actions: [OPEN, DELETE],
    };
    paintModule(painter, module([source]));

    expect(seen).toEqual([{ ...source, actions: [OPEN] }]);
  });
});

describe('paintPanel', () => {
  /* `c1` carries actions on purpose: an actionless row could not tell a `paintPanel` that bypassed
       `paintModule`'s capability filter from one that delegates. */
  const view: TrackPageView = {
    rowModules: [
      { key: 'cards', title: 'Cards', rows: [row('c1', 'One', [OPEN, DELETE])], empty: 'No cards yet.' },
      { key: 'tasks', title: 'Tasks', rows: [], empty: 'No tasks declared yet.' },
    ],
  };

  it('paints every row module, in the view model’s order', () => {
    const { painter, calls } = recordingPainter();
    const painted = paintPanel(painter, view);

    expect(calls.modules.map((m) => m.key)).toEqual(['cards', 'tasks']);
    expect(painted).toEqual([
      'module:cards(row:c1)',
      'module:tasks(empty:No tasks declared yet.)',
    ]);
  });

  it('applies the painter’s capability filter on the `paintPanel` path too', () => {
    const { painter, calls } = recordingPainter({
      ...ALL_SUPPORTED,
      'delete-card': { supported: false, why: 'this host passed no delete handler' },
    });
    paintPanel(painter, view);

    expect(calls.rows).toEqual(['c1']);
    expect(calls.actions).toEqual([[OPEN]]);
  });

  it('paints nothing for a view with no row modules', () => {
    const { painter, calls } = recordingPainter();

    expect(paintPanel(painter, { rowModules: [] })).toEqual([]);
    expect(calls.modules).toEqual([]);
  });
});

/* One authority for marker names: the stylesheet and the projection check both key off these spellings. */
describe('DOM marker vocabulary', () => {
  it('names every marker attribute exactly', () => {
    expect(MARKER).toEqual({
      module: 'data-nc-module',
      row: 'data-nc-row',
      badge: 'data-nc-badge',
      action: 'data-nc-row-action',
      status: 'data-nc-status',
      field: 'data-nc-field',
    });
  });

  it('names every `data-nc-field` value exactly', () => {
    expect(FIELD).toEqual({
      title: 'title',
      kind: 'kind',
      moduleTitle: 'module-title',
      empty: 'empty',
    });
  });

  it('spells the status marker at its post-rename name', () => {
    expect(MARKER.status).toBe('data-nc-status');
  });
});
