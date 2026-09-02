import { describe, expect, it } from 'vitest';

import { paintModule, type PanelRow, type RowModuleView, type RowPainter } from './panel.js';

function row(id: string, title: string): PanelRow {
  return { id, title, kind: null, badges: [], status: null, actions: [] };
}

function module(rows: readonly PanelRow[]): RowModuleView {
  return { key: 'cards', title: 'Cards', rows, empty: 'No cards yet.' };
}

/** Records which leaf constructors ran, which is the property under test: the
 *  empty state is exclusive with the row state. */
function recordingPainter(): {
  painter: RowPainter<string>;
  calls: { rows: string[]; empties: string[]; modules: { key: string; title: string; children: readonly string[] }[] };
} {
  const calls = {
    rows: [] as string[],
    empties: [] as string[],
    modules: [] as { key: string; title: string; children: readonly string[] }[],
  };
  const painter: RowPainter<string> = {
    row: (value) => { calls.rows.push(value.id); return `row:${value.id}`; },
    empty: (text) => { calls.empties.push(text); return `empty:${text}`; },
    module: (parts) => { calls.modules.push(parts); return `module:${parts.key}(${parts.children.join(',')})`; },
    action: {
      'reveal-block': { supported: true },
      'open-card': { supported: true },
      'delete-card': { supported: true },
    },
  };
  return { painter, calls };
}

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
