// #1234 S1b-2 — the contract of `tools/projection`'s `checkProjection`, driven
// by synthetic painters.
//
// **This suite tests the checker in `tools/`, not this feature.** It does not
// touch `public.tsx` and depends on nothing private to `features/wave`; every
// painter here is written in this file and emits marked JSX directly, so no
// `ui/` primitive is involved either. It lives under `web/src` for one reason:
// `vitest.config.ts` (readonly) pins `tools/**/*.test.ts` to the **node**
// environment, and only `web/src/**` runs in jsdom. The alternatives were
// editing that readonly config plus the TypeScript project boundary, or paying
// Chromium for a pure-DOM contract.
//
// **Every fixture shares one canonical `mount`.** The checker cannot prove its
// `mount` faithful — one that ignored `painted` and fabricated a correct tree
// would pass everything — so this suite removes the freedom rather than
// pretending to check it, and the last describe pins that this particular mount
// is not ignoring its argument.
//
// **Each malicious painter asserts `toEqual`, not `toContain`.** "Something
// went red" is not evidence that the obligation under test is the one that
// caught it. Where two obligations are entangled by construction the expected
// array holds both codes and says why; there are exactly two such places
// (badge nesting, and status co-hosted on the row element).

import { render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { describe, expect, it } from 'vitest';

import { FIELD, MARKER } from '../../../../../core/view/panel.ts';
import type {
  ActionSupport, PanelRow, RowAction, RowModuleView, RowPainter,
} from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import type { ProjectionNode, ViolationCode } from '../../../../../tools/projection/public.ts';

/** The one `mount` every fixture in this file uses. */
const mount = (painted: readonly ReactNode[]): ProjectionNode => render(<>{painted}</>).container;

const codesOf = (
  painter: RowPainter<ReactNode>,
  modules: readonly RowModuleView[],
): readonly ViolationCode[] => checkProjection(painter, modules, mount).map((violation) => violation.code);

// ── The view model under test ────────────────────────────────────────────────

const allSupported: RowPainter<ReactNode>['action'] = Object.freeze({
  'reveal-block': Object.freeze<ActionSupport>({ supported: true }),
  'open-card': Object.freeze<ActionSupport>({ supported: true }),
  'delete-card': Object.freeze<ActionSupport>({ supported: true }),
});

const deleteUnsupported: RowPainter<ReactNode>['action'] = Object.freeze({
  ...allSupported,
  'delete-card': Object.freeze<ActionSupport>({ supported: false, why: 'the host passed no delete handler' }),
});

const deleteAction: RowAction = Object.freeze({
  kind: 'delete-card', cardId: 'c1', label: 'Delete card Ingest', hint: 'Delete card',
});

const cardRow: PanelRow = Object.freeze({
  id: 'c1',
  title: 'Ingest',
  kind: 'worker',
  badges: Object.freeze([Object.freeze({ id: 'b1', text: 'kernel-owned', struck: false })]),
  status: null,
  actions: Object.freeze<readonly RowAction[]>([
    Object.freeze({ kind: 'open-card', cardId: 'c1', label: null, hint: null }),
    deleteAction,
  ]),
});

const plainRow: PanelRow = Object.freeze({
  id: 'c2', title: 'Sweep', kind: null, badges: Object.freeze([]), status: null, actions: Object.freeze([]),
});

const taskRow: PanelRow = Object.freeze({
  id: 't1',
  title: 'T-1',
  kind: 'review',
  badges: Object.freeze([
    Object.freeze({ id: 'd1', text: 'declared', struck: false }),
    Object.freeze({ id: 'd2', text: 'declared', struck: true }),
  ]),
  status: Object.freeze({ token: 'dispatched', phrase: 'Dispatched a moment ago' }),
  actions: Object.freeze<readonly RowAction[]>([
    Object.freeze({ kind: 'reveal-block', blockId: 'k', label: null, hint: 'Show T-1 in the report' }),
  ]),
});

const cards: RowModuleView = Object.freeze({
  key: 'cards', title: 'Cards', empty: 'No cards yet', rows: Object.freeze([cardRow, plainRow]),
});
const tasks: RowModuleView = Object.freeze({
  key: 'tasks', title: 'Tasks', empty: 'No tasks yet', rows: Object.freeze([taskRow]),
});
const emptyTasks: RowModuleView = Object.freeze({ ...tasks, rows: Object.freeze([]) });

const view: readonly RowModuleView[] = Object.freeze([cards, tasks]);
const withEmpty: readonly RowModuleView[] = Object.freeze([cards, emptyTasks]);

// ── Marked JSX, out of which every painter below is composed ─────────────────

const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

const titleField = (text: string): ReactNode => (
  <span key="title" {...mark(MARKER.field, FIELD.title)}>{text}</span>
);
const kindField = (text: string): ReactNode => (
  <span key="kind" {...mark(MARKER.field, FIELD.kind)}>{text}</span>
);
const badgeEl = (id: string, text: string, body?: ReactNode): ReactNode => (
  <span key={`badge-${id}`} {...mark(MARKER.badge, id)}>{body ?? text}</span>
);
const statusEl = (token: string, phrase: string): ReactNode => (
  <span key="status" {...mark(MARKER.status, token)} title={phrase} />
);
/** Action hosts are spans, not buttons: §6.3 already declines to check whether
 *  a marker host is interactive, and one variant below nests two of them. */
const actionEl = (
  action: RowAction,
  over: Readonly<{ label?: string | null; hint?: string | null; body?: ReactNode }> = {},
): ReactNode => {
  const label = over.label === undefined ? action.label : over.label;
  const hint = over.hint === undefined ? action.hint : over.hint;
  return (
    <span
      key={`action-${action.kind}`}
      {...mark(MARKER.action, action.kind)}
      {...(label === null ? {} : { 'aria-label': label })}
      {...(hint === null ? {} : { title: hint })}
    >{over.body}</span>
  );
};

type RowParts = Readonly<{
  title: ReactNode; kind: ReactNode; badges: readonly ReactNode[]; status: ReactNode; actions: readonly ReactNode[];
}>;

const partsOf = (row: PanelRow): RowParts => ({
  title: titleField(row.title),
  kind: row.kind === null ? null : kindField(row.kind),
  badges: row.badges.map((badge) => badgeEl(badge.id, badge.text)),
  status: row.status === null ? null : statusEl(row.status.token, row.status.phrase),
  actions: row.actions.map((action) => actionEl(action)),
});

const bodyOf = (parts: RowParts): ReactNode => (
  <>{parts.title}{parts.kind}{parts.badges}{parts.status}{parts.actions}</>
);

const rowEl = (row: PanelRow, body: ReactNode, key = row.id): ReactNode => (
  <div key={key} {...mark(MARKER.row, row.id)}>{body}</div>
);

type ModuleParts = Parameters<RowPainter<ReactNode>['module']>[0];

const moduleEl = (parts: ModuleParts, body: ReactNode, key: string = parts.key): ReactNode => (
  <section key={key} {...mark(MARKER.module, parts.key)}>{body}</section>
);

const moduleTitleEl = (title: string): ReactNode => (
  <h2 key="module-title" {...mark(MARKER.field, FIELD.moduleTitle)}>{title}</h2>
);

const emptyEl = (text: string): ReactNode => (
  <span key="empty" {...mark(MARKER.field, FIELD.empty)}>{text}</span>
);

const moduleBody = (parts: ModuleParts): ReactNode => (
  <>{moduleTitleEl(parts.title)}{parts.children}</>
);

/** The painter every malicious variant below is a one-part override of. */
const faithful: RowPainter<ReactNode> = Object.freeze({
  action: allSupported,
  empty: (text) => emptyEl(text),
  module: (parts) => moduleEl(parts, moduleBody(parts)),
  row: (row) => rowEl(row, bodyOf(partsOf(row))),
});

const variant = (over: Partial<RowPainter<ReactNode>>): RowPainter<ReactNode> => ({ ...faithful, ...over });

/** A painter whose rows are faithful except for `over`, applied to one row. */
const rowVariant = (id: string, build: (row: PanelRow, parts: RowParts) => ReactNode): RowPainter<ReactNode> =>
  variant({ row: (row) => (row.id === id ? build(row, partsOf(row)) : faithful.row(row)) });

/** A painter whose modules are faithful except for `over`, applied to one module. */
const moduleVariant = (
  key: RowModuleView['key'],
  build: (parts: ModuleParts) => ReactNode,
): RowPainter<ReactNode> =>
  variant({ module: (parts) => (parts.key === key ? build(parts) : faithful.module(parts)) });

// ─────────────────────────────────────────────────────────────────────────────
// A — the four-layer bijection
// ─────────────────────────────────────────────────────────────────────────────

describe('A / module layer', () => {
  it('module-sequence: a module is dropped', () => {
    expect(codesOf(moduleVariant('tasks', () => null), view)).toEqual(['module-sequence']);
  });

  it('module-sequence: the keys are transposed', () => {
    const painter = variant({
      module: (parts) => (
        <section key={parts.key} {...mark(MARKER.module, parts.key === 'cards' ? 'tasks' : 'cards')}>
          {moduleBody(parts)}
        </section>
      ),
    });
    expect(codesOf(painter, view)).toEqual(['module-sequence']);
  });

  it('module-sequence: a module is duplicated in place', () => {
    const painter = moduleVariant('tasks', (parts) => (
      <>{moduleEl(parts, moduleBody(parts))}{moduleEl(parts, moduleBody(parts), 'copy')}</>
    ));
    expect(codesOf(painter, view)).toEqual(['module-sequence']);
  });

  it('module-nesting: the second module is painted inside the first', () => {
    // The nested module is the empty one, painted in full inside `cards`, so the
    // flat key sequence is still `['cards', 'tasks']` and only the nesting is wrong.
    const painter = variant({
      module: (parts) => (parts.key === 'tasks' ? null : (
        <section key={parts.key} {...mark(MARKER.module, parts.key)}>
          {moduleBody(parts)}
          <section {...mark(MARKER.module, 'tasks')}>
            {moduleTitleEl(emptyTasks.title)}{emptyEl(emptyTasks.empty)}
          </section>
        </section>
      )),
    });
    expect(codesOf(painter, withEmpty)).toEqual(['module-nesting']);
  });

  // There is no `module-partition` case, and none can exist: the module layer's
  // enclosing container is the root itself, so "root total == sum over enclosing
  // containers" is an identity. See the note in `checkTree`.
});

describe('A / row layer', () => {
  it('row-sequence: a row is dropped', () => {
    expect(codesOf(rowVariant('c2', () => null), view)).toEqual(['row-sequence']);
  });

  it('row-sequence: two row ids are transposed', () => {
    const painter = variant({
      row: (row) => (
        <div key={row.id} {...mark(MARKER.row, row.id === 'c1' ? 'c2' : row.id === 'c2' ? 'c1' : row.id)}>
          {bodyOf(partsOf(row))}
        </div>
      ),
    });
    expect(codesOf(painter, view)).toEqual(['row-sequence']);
  });

  it('row-sequence: a row is duplicated in place', () => {
    const painter = rowVariant('c1', (row, parts) => (
      <>{rowEl(row, bodyOf(parts))}{rowEl(row, bodyOf(parts), 'copy')}</>
    ));
    expect(codesOf(painter, view)).toEqual(['row-sequence']);
  });

  it('row-nesting: the second row is painted inside the first', () => {
    const painter = variant({
      row: (row) => {
        if (row.id === 'c2') return null;
        if (row.id !== 'c1') return faithful.row(row);
        return rowEl(row, <>{bodyOf(partsOf(row))}{rowEl(plainRow, bodyOf(partsOf(plainRow)))}</>);
      },
    });
    expect(codesOf(painter, view)).toEqual(['row-nesting']);
  });

  it('row-partition: a copied row is painted outside the module container', () => {
    const painter = moduleVariant('cards', (parts) => (
      <>{moduleEl(parts, moduleBody(parts))}<div key="stray" {...mark(MARKER.row, 'c1')} /></>
    ));
    expect(codesOf(painter, view)).toEqual(['row-partition']);
  });
});

describe('A / badge layer', () => {
  it('badge-sequence: a badge is dropped', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges.slice(0, 1)}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-sequence']);
  });

  it('badge-sequence: two badges are transposed', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{[...parts.badges].reverse()}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-sequence']);
  });

  it('badge-sequence: a badge is duplicated in place', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}
        <span key="copy" {...mark(MARKER.badge, 'd1')}>declared</span>
        {parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-sequence']);
  });

  it('badge-nesting: one badge is painted inside another', () => {
    // Entangled by construction: a badge inside a badge *is* a badge carrier
    // holding a descendant content marker, so `carrier-not-leaf` necessarily
    // fires with it. Both codes are asserted rather than relaxing to `toContain`.
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}
        {badgeEl('d1', 'declared', <>declared{badgeEl('d2', 'declared')}</>)}
        {parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-nesting', 'carrier-not-leaf']);
  });

  it('badge-partition: a copied badge is painted outside the row container', () => {
    const painter = moduleVariant('tasks', (parts) => moduleEl(parts, (
      <>{moduleBody(parts)}<span key="stray" {...mark(MARKER.badge, 'd1')}>declared</span></>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-partition']);
  });

  it('badge-text: a badge carries the wrong text', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}
        {badgeEl('d1', 'declaredx')}{parts.badges[1]}
        {parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['badge-text']);
  });
});

describe('A / action layer, and D — the action wording', () => {
  it('action-sequence: an unsupported action is painted anyway', () => {
    const painter = variant({
      action: deleteUnsupported,
      row: (row) => (row.id !== 'c1' ? faithful.row(row) : rowEl(row, (
        <>{bodyOf(partsOf(row))}{actionEl(deleteAction)}</>
      ))),
    });
    expect(codesOf(painter, view)).toEqual(['action-sequence']);
  });

  it('action-sequence: a supported action is skipped', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}{parts.actions.slice(0, 1)}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-sequence']);
  });

  it('action-sequence: two actions are transposed', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}{[...parts.actions].reverse()}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-sequence']);
  });

  it('action-nesting: one action host is painted inside another', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        {actionEl(row.actions[0], { body: actionEl(row.actions[1]) })}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-nesting']);
  });

  it('action-partition: a copied action host is painted outside the row container', () => {
    const painter = moduleVariant('cards', (parts) => moduleEl(parts, (
      <>{moduleBody(parts)}<span key="stray" {...mark(MARKER.action, 'open-card')} /></>
    )));
    expect(codesOf(painter, view)).toEqual(['action-partition']);
  });

  it('action-label: a null label is given an aria-label anyway', () => {
    // The real regression: a second accessible name overrides the control's
    // visible text (WCAG 2.5.3). A one-sided check would not see it.
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        {actionEl(row.actions[0], { label: 'Open' })}{parts.actions[1]}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-label']);
  });

  it('action-label: a non-null label is worded differently', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        {parts.actions[0]}{actionEl(row.actions[1], { label: 'Delete' })}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-label']);
  });

  it('action-hint: a null hint is given a title anyway', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        {actionEl(row.actions[0], { hint: 'Open the card' })}{parts.actions[1]}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-hint']);
  });

  it('action-hint: a non-null hint is worded differently', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        {actionEl(row.actions[0], { hint: 'Show in report' })}</>
    )));
    expect(codesOf(painter, view)).toEqual(['action-hint']);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// B — status
// ─────────────────────────────────────────────────────────────────────────────

describe('B / status', () => {
  it('status-cardinality: two status elements in one row', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{parts.status}
        <span key="copy" {...mark(MARKER.status, 'dispatched')} title="Dispatched a moment ago" />
        {parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['status-cardinality']);
  });

  it('status-cardinality: a status-less row is given one', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}{statusEl('ready', 'Ready')}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['status-cardinality']);
  });

  it('status-partition: a copied status is painted outside the row container', () => {
    const painter = moduleVariant('tasks', (parts) => moduleEl(parts, (
      <>{moduleBody(parts)}
        <span key="stray" {...mark(MARKER.status, 'dispatched')} title="Dispatched a moment ago" /></>
    )));
    expect(codesOf(painter, view)).toEqual(['status-partition']);
  });

  it('status-token: the phrase is written into the token attribute', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}
        {statusEl('Dispatched a moment ago', 'Dispatched a moment ago')}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['status-token']);
  });

  it('status-phrase: the painter words the phrase itself', () => {
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>{parts.title}{parts.kind}{parts.badges}
        {statusEl('dispatched', 'Status: Dispatched a moment ago')}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['status-phrase']);
  });

  it('a status marker co-hosted on the row element is caught twice over', () => {
    // Entangled by construction, and both halves are the point: the row element
    // carries two content markers (E), and the row then owns zero status
    // descendants (B). Asserted as the exact pair rather than relaxed.
    const painter = rowVariant('t1', (row, parts) => (
      <div
        key={row.id}
        {...mark(MARKER.row, row.id)}
        {...mark(MARKER.status, 'dispatched')}
        title="Dispatched a moment ago"
      >{parts.title}{parts.kind}{parts.badges}{parts.actions}</div>
    ));
    expect(codesOf(painter, view)).toEqual(['marker-co-host', 'status-cardinality']);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// C — the field carriers
// ─────────────────────────────────────────────────────────────────────────────

describe('C / field carriers', () => {
  it('field-cardinality: the row title carrier is missing', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{row.title}{parts.kind}{parts.badges}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-cardinality']);
  });

  it('field-cardinality: the row title is painted twice', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}<span key="again" {...mark(MARKER.field, FIELD.title)}>{row.title}</span>
        {parts.kind}{parts.badges}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-cardinality']);
  });

  it('field-text: the row title carrier holds the wrong string', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{titleField('Ingest ')}{parts.kind}{parts.badges}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-text']);
  });

  it('field-cardinality: a null kind is given a carrier', () => {
    const painter = rowVariant('c2', (row, parts) => rowEl(row, (
      <>{parts.title}{kindField('card')}{parts.badges}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-cardinality']);
  });

  it('field-text: the kind carrier holds the title instead', () => {
    const painter = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{parts.title}{kindField(row.title)}{parts.badges}{parts.status}{parts.actions}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-text']);
  });

  it('field-text: the module title carrier holds the wrong string', () => {
    const painter = moduleVariant('cards', (parts) => moduleEl(parts, (
      <>{moduleTitleEl('Cards ')}{parts.children}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-text']);
  });

  it('field-cardinality: the module title is painted twice', () => {
    const painter = moduleVariant('cards', (parts) => moduleEl(parts, (
      <>{moduleTitleEl(parts.title)}
        <h2 key="again" {...mark(MARKER.field, FIELD.moduleTitle)}>{parts.title}</h2>
        {parts.children}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-cardinality']);
  });

  it('field-cardinality: a populated module also paints the empty text', () => {
    const painter = moduleVariant('cards', (parts) => moduleEl(parts, (
      <>{moduleBody(parts)}{emptyEl(cards.empty)}</>
    )));
    expect(codesOf(painter, view)).toEqual(['field-cardinality']);
  });

  it('field-cardinality: an empty module paints the text without its carrier', () => {
    expect(codesOf(variant({ empty: (text) => <span key="empty">{text}</span> }), withEmpty))
      .toEqual(['field-cardinality']);
  });

  it('field-text: an empty module words the empty text itself', () => {
    expect(codesOf(variant({ empty: () => emptyEl('Nothing here') }), withEmpty)).toEqual(['field-text']);
  });

  it('field-partition: a copied title carrier is painted outside the row container', () => {
    const painter = moduleVariant('cards', (parts) => moduleEl(parts, (
      <>{moduleBody(parts)}<span key="stray" {...mark(MARKER.field, FIELD.title)}>Ingest</span></>
    )));
    expect(codesOf(painter, view)).toEqual(['field-partition']);
  });

  it('carrier-not-leaf: a content marker is painted inside a field carrier', () => {
    // The badge is the row's real, single badge — the bijection is intact, so
    // only the leaf rule can catch this. Without it, `textContent` comparison on
    // a carrier holding other fields' text is meaningless.
    const painter = rowVariant('t1', (row, parts) => rowEl(row, (
      <>
        <span key="title" {...mark(MARKER.field, FIELD.title)}>{row.title}{badgeEl('d1', 'declared')}</span>
        {parts.kind}{parts.badges[1]}{parts.status}{parts.actions}
      </>
    )));
    expect(codesOf(painter, view)).toEqual(['carrier-not-leaf']);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// E — one content marker per element
// ─────────────────────────────────────────────────────────────────────────────

describe('E / co-hosting', () => {
  // A view built so that a badge's text happens to equal the row's kind: that is
  // what lets one element satisfy both obligations and makes `marker-co-host`
  // the *only* thing standing between the checker and a false green.
  const cohostRow: PanelRow = Object.freeze({
    id: 'x1',
    title: 'Xenon',
    kind: 'tag',
    badges: Object.freeze([Object.freeze({ id: 'g1', text: 'tag', struck: false })]),
    status: null,
    actions: Object.freeze([]),
  });
  const cohostView: readonly RowModuleView[] = Object.freeze([Object.freeze({
    key: 'cards', title: 'Cards', empty: 'No cards yet', rows: Object.freeze([cohostRow]),
  })]);

  it('marker-co-host: one element serves as both the badge and the kind carrier', () => {
    const painter = variant({
      row: (row) => rowEl(row, (
        <>{titleField(row.title)}
          <span key="both" {...mark(MARKER.badge, 'g1')} {...mark(MARKER.field, FIELD.kind)}>tag</span></>
      )),
    });
    expect(codesOf(painter, cohostView)).toEqual(['marker-co-host']);
  });

  it('the same shape with two elements is green', () => {
    expect(codesOf(faithful, cohostView)).toEqual([]);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// The false-red side. v4 and the v7 draft both died having only proved that
// bad painters go red.
// ─────────────────────────────────────────────────────────────────────────────

describe('a faithful painter is green', () => {
  it('on a populated panel with a title != kind row, badges, a status and two actions', () => {
    expect(checkProjection(faithful, view, mount)).toEqual([]);
  });

  it('on a module with zero rows', () => {
    expect(checkProjection(faithful, withEmpty, mount)).toEqual([]);
  });

  it('when the painter invents chrome of its own — projection is not required to be onto', () => {
    const painter = variant({
      module: (parts) => moduleEl(parts, (
        <>
          <div key="chrome">Everything in this wave</div>
          {moduleBody(parts)}
          <footer key="foot">{parts.children.length} shown</footer>
        </>
      )),
      row: (row) => rowEl(row, (
        <>
          <span key="bullet">•</span>
          {bodyOf(partsOf(row))}
          <button key="extra" type="button">More…</button>
        </>
      )),
    });
    expect(checkProjection(painter, view, mount)).toEqual([]);
  });

  it('when an action is unsupported and correspondingly not painted', () => {
    // The mirror of the `action-sequence` case above: the capability filter must
    // not make the *absence* of an unsupported action go red.
    expect(codesOf(variant({ action: deleteUnsupported }), view)).toEqual([]);
  });
});

// ─────────────────────────────────────────────────────────────────────────────
// The mount is a trust boundary. This is the little that can be shown about it.
// ─────────────────────────────────────────────────────────────────────────────

describe('mount renders what the painter painted', () => {
  /*
   * `checkProjection` cannot prove its `mount` faithful — a `mount` that dropped
   * `painted` on the floor and returned a fabricated correct tree would pass
   * every case above, and no assertion inside the checker can tell the two
   * apart. What *can* be shown, and is shown here, is that **this** mount is not
   * doing that: two painters that differ only in what they paint, over the same
   * modules and through the same mount, produce different verdicts. A mount that
   * ignored `painted` would produce the same verdict for both.
   *
   * This is evidence about the mount, not about the checker's coverage, and it
   * does not extend to the painter factories S1b-3/4 build in production.
   */
  const identifiable = 'ONLY-THIS-PAINTER-WRITES-THIS';

  it('the verdict is a function of the painted output', () => {
    const marked = rowVariant('c1', (row, parts) => rowEl(row, (
      <>{titleField(identifiable)}{parts.kind}{parts.badges}{parts.status}{parts.actions}</>
    )));
    const faithfulVerdict = checkProjection(faithful, view, mount);
    const markedVerdict = checkProjection(marked, view, mount);

    expect(faithfulVerdict).toEqual([]);
    expect(markedVerdict.map((violation) => violation.code)).toEqual(['field-text']);
    expect(markedVerdict[0].detail).toContain(identifiable);
  });
});
