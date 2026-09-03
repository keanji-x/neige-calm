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
  return { id, title, kind: null, badges: [], status: null, actions };
}

function module(rows: readonly PanelRow[]): RowModuleView {
  return { key: 'cards', title: 'Cards', rows, empty: 'No cards yet.' };
}

const ALL_SUPPORTED: Readonly<Record<RowAction['kind'], ActionSupport>> = {
  'reveal-block': { supported: true },
  'open-card': { supported: true },
  'delete-card': { supported: true },
};

/** Records which leaf constructors ran, and with what, which is the property
 *  under test: the empty state is exclusive with the row state, and the
 *  capability table decides which actions `row()` is even handed. */
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

const REVEAL: RowAction = { kind: 'reveal-block', blockId: 'b1', label: null, hint: 'Show b1' };
const OPEN: RowAction = { kind: 'open-card', cardId: 'c1', label: null, hint: null };
const DELETE: RowAction = { kind: 'delete-card', cardId: 'c1', label: 'Delete card One', hint: 'Delete card' };

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

/*
 * The capability table has to change what `row()` sees, or it is a decoration:
 * S1a's type demanded it and nothing read it, so a painter could declare
 * `delete-card` unsupported and paint one anyway with nothing to notice.
 *
 * The guarantee is deliberately narrow — it constrains the actions handed to
 * `row()`, and that is the whole of what *this suite* observes. S1b-2's
 * `checkProjection` now carries the consequence at the marker level: how many
 * `[data-nc-row-action]` markers a painter emits is constrained too, for the
 * painters it is run over — which since S1b-3b include a production one, the
 * desktop painter, over the real rendered page, and since S1b-4a/4b the mobile
 * painter over both of its drill-down pages
 * (`track/page/mobile-projection.test.tsx`). Neither claim says an unsupported
 * control cannot be drawn: a painter may draw an extra control that carries no
 * marker at all.
 */
describe('paintModule action filtering', () => {
  /*
   * The false-red guard. Proving only that a bad painter goes red proves
   * nothing about a good one: a filter that dropped everything, or one applied
   * to the wrong list, would satisfy every negative test above and break every
   * real painter. So an all-supported table must pass the actions through
   * **unchanged — neither fewer nor more, and in order**.
   */
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

  /* The filter must not disturb the rest of the row it copies. */
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
      actions: [OPEN, DELETE],
    };
    paintModule(painter, module([source]));

    expect(seen).toEqual([{ ...source, actions: [OPEN] }]);
  });
});

/*
 * `paintPanel` is the **desktop's** traversal: the desktop panel card lays
 * both modules out in one tree. Mobile drills into one module at a time and,
 * since S1b-4a/4b, calls `paintModule` once per page, so the module sequence
 * there is a navigation structure, not a DOM sequence. The mobile surface
 * therefore calls `paintModule` and — correctly — never `paintPanel`.
 *
 * **The desktop does, since S1b-3b.** The production chain is
 * `track/page/public.tsx`'s desktop panel card → `paintDesktopPanel` →
 * `paintPanel` → `paintModule`. So outside this suite `paintPanel`'s callers are
 * that wrapper and `checkProjection` (`tools/projection/public.ts`).
 *
 * Nothing *in this file* forces that chain to exist — this suite would stay
 * green if the page stopped calling the wrapper tomorrow. What holds it is
 * `track/page/desktop-entry.test.tsx`, which mocks `paintDesktopPanel` and
 * checks the page both calls it and renders what it returns; the residue that
 * oracle leaves is written down in `tools/projection/public.ts`'s standing list.
 */
describe('paintPanel', () => {
  /* `c1` carries actions on purpose. With an actionless row here, `paintPanel`
     could be rewritten to walk the modules itself — calling `painter.row`
     directly and skipping `paintModule`'s capability filter entirely — and
     every assertion in this block would stay green, because the only row would
     get `actions: []` either way. The capability filter must not be reachable
     only through `paintModule`'s own tests. */
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

  /* The capability filter is not bypassable by going through `paintPanel`.
     A `paintPanel` that inlined the traversal instead of delegating to
     `paintModule` would hand `row()` the unfiltered `[OPEN, DELETE]`, and this
     is the only assertion in the suite that would notice. */
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

/*
 * The marker names, pinned as a whole table.
 *
 * A marker name drifts silently by construction: the stylesheet keys off one
 * spelling, the projection check off another, and each side is green on its
 * own (§3.4). One authority, one assertion.
 */
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

  /* The status marker is defined at its **final** name, and since S1b-3a that
     is also the name `public.tsx` and `page.module.css` write: the earlier
     spelling of this attribute is gone from the tree. */
  it('spells the status marker at its post-rename name', () => {
    expect(MARKER.status).toBe('data-nc-status');
  });
});
