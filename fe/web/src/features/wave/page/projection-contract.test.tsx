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
// `mount` faithful — a `mount` that ignored `painted` and fabricated a correct
// tree would pass everything — so the suite removes the freedom instead of
// pretending to check it, and `mount renders what the painter painted` below
// pins that this particular one is not ignoring its argument.

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

const supportedTable: RowPainter<ReactNode>['action'] = Object.freeze({
  'reveal-block': Object.freeze<ActionSupport>({ supported: true }),
  'open-card': Object.freeze<ActionSupport>({ supported: true }),
  'delete-card': Object.freeze<ActionSupport>({ supported: true }),
});

const cardRow: PanelRow = Object.freeze({
  id: 'c1',
  title: 'Ingest',
  kind: 'worker',
  badges: Object.freeze([Object.freeze({ id: 'b1', text: 'kernel-owned', struck: false })]),
  status: null,
  actions: Object.freeze<readonly RowAction[]>([
    Object.freeze({ kind: 'open-card', cardId: 'c1', label: null, hint: null }),
    Object.freeze({ kind: 'delete-card', cardId: 'c1', label: 'Delete card Ingest', hint: 'Delete card' }),
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
const view: readonly RowModuleView[] = Object.freeze([cards, tasks]);

// ── Marked JSX, out of which every painter below is composed ─────────────────

const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

const titleField = (text: string): ReactNode => (
  <span key="title" {...mark(MARKER.field, FIELD.title)}>{text}</span>
);
const kindField = (text: string): ReactNode => (
  <span key="kind" {...mark(MARKER.field, FIELD.kind)}>{text}</span>
);
const badgeEl = (id: string, text: string): ReactNode => (
  <span key={`badge-${id}`} {...mark(MARKER.badge, id)}>{text}</span>
);
const statusEl = (token: string, phrase: string): ReactNode => (
  <span key="status" {...mark(MARKER.status, token)} title={phrase} />
);
const actionEl = (action: RowAction, children?: ReactNode): ReactNode => (
  <button
    key={`action-${action.kind}`}
    type="button"
    {...mark(MARKER.action, action.kind)}
    {...(action.label === null ? {} : { 'aria-label': action.label })}
    {...(action.hint === null ? {} : { title: action.hint })}
  >{children}</button>
);

const rowBody = (row: PanelRow): ReactNode => (
  <>
    {titleField(row.title)}
    {row.kind === null ? null : kindField(row.kind)}
    {row.badges.map((badge) => badgeEl(badge.id, badge.text))}
    {row.status === null ? null : statusEl(row.status.token, row.status.phrase)}
    {row.actions.map((action) => actionEl(action))}
  </>
);

const rowEl = (row: PanelRow, body: ReactNode): ReactNode => (
  <div key={row.id} {...mark(MARKER.row, row.id)}>{body}</div>
);

type ModuleParts = Parameters<RowPainter<ReactNode>['module']>[0];

const moduleEl = (parts: ModuleParts, body: ReactNode): ReactNode => (
  <section key={parts.key} {...mark(MARKER.module, parts.key)}>{body}</section>
);

const moduleBody = (parts: ModuleParts): ReactNode => (
  <>
    <h2 {...mark(MARKER.field, FIELD.moduleTitle)}>{parts.title}</h2>
    {parts.children}
  </>
);

/** The painter every malicious variant below is a one-part override of. */
const faithful: RowPainter<ReactNode> = Object.freeze({
  action: supportedTable,
  empty: (text) => <span key="empty" {...mark(MARKER.field, FIELD.empty)}>{text}</span>,
  module: (parts) => moduleEl(parts, moduleBody(parts)),
  row: (row) => rowEl(row, rowBody(row)),
});

const variant = (over: Partial<RowPainter<ReactNode>>): RowPainter<ReactNode> => ({ ...faithful, ...over });

// ─────────────────────────────────────────────────────────────────────────────

describe('checkProjection — a faithful painter', () => {
  it('reports nothing for a populated panel', () => {
    expect(checkProjection(faithful, view, mount)).toEqual([]);
  });
});

describe('checkProjection — module layer', () => {
  it('reports module-sequence when a module is dropped', () => {
    const painter = variant({ module: (parts) => (parts.key === 'tasks' ? null : moduleEl(parts, moduleBody(parts))) });
    expect(codesOf(painter, view)).toEqual(['module-sequence']);
  });
});
