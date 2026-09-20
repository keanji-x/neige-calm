// The desktop track panel, painted from `core/view`'s panel view model.
// A row or empty leaf is pending until its module says which module it lands in; a module leaf is finished.

import { ListText } from '../../../ui/list-typography/public.tsx';
import { Fragment, type ReactNode } from 'react';

import { inventorySections, panelRowGroup, type InventoryGroupKey } from '../../../../../core/view/panel-groups.ts';
import { InventoryGroups } from './inventory-groups.tsx';
import { FIELD, MARKER, paintPanel } from '../../../../../core/view/panel.ts';
import type {
  ActionSupport, PanelRow, RowAction, RowBadge, RowModuleView, RowPainter, TrackPageView,
} from '../../../../../core/view/panel.ts';
import { activityLabelOf } from '../../../../../core/domain/activity.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import { PanelEmpty, PanelModule } from '../../../ui/panel-card/public.tsx';
import styles from './page.module.css';

/** A row or an empty line, painted as far as it can be here; `paint` is called from `module()` and nowhere else. */
type PendingLeaf = Readonly<{
  slot: 'row' | 'empty';
  group: (moduleKey: RowModuleView['key']) => InventoryGroupKey;
  paint: (moduleKey: RowModuleView['key']) => ReactNode;
}>;

/** A module, which needs nothing further. */
type ModuleLeaf = Readonly<{ slot: 'module'; node: ReactNode }>;

/** A tagged union: only a pending leaf ever needs a key, and it never reaches the top level. */
export type DesktopLeaf = PendingLeaf | ModuleLeaf;

export type DesktopPainterDeps = Readonly<{
  onOpenCard?: (cardId: string) => void;
  onOpenTask?: (blockId: string) => void;
  onDeleteCard?: (cardId: string) => void;
  /** The Cards module head's `+`, composed by `app/router`; travels as an opaque node. */
  cardsAction?: ReactNode;
  /** Compact, already-derived progress for the Tasks module head. */
  taskSummary?: string | null;
  /** An app-composed, non-projection module placed beside the Cards module. */
  afterCards?: ReactNode;
}>;

/** One marker attribute, spelled from `MARKER` / `FIELD` so no name is retyped here. */
const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

/** What a painter needs from one of a row's actions. `null` when absent — never derived, or filtered by the capability table — and both mean draw no control. */
type Control = Readonly<{
  id: string;
  label: string | null;
  hint: string | null;
  description: string | null;
}>;

function control(row: PanelRow, kind: RowAction['kind']): Control | null {
  for (const action of row.actions) {
    if (action.kind !== kind) continue;
    return {
      id: action.kind === 'reveal-block' ? action.blockId : action.cardId,
      label: action.label,
      hint: action.hint,
      description: action.description,
    };
  }
  return null;
}

/** `null` must emit no attribute at all: a second accessible name overrides a control's visible text (WCAG 2.5.3). */
function wording(action: Control): Readonly<Record<string, string>> {
  return {
    ...(action.label === null ? {} : { 'aria-label': action.label }),
    ...(action.hint === null ? {} : { title: action.hint }),
    ...(action.description === null ? {} : { 'aria-description': action.description }),
  };
}

function cardBadge(badge: RowBadge): ReactNode {
  return (
    <ListText tone="secondary" key={badge.id} className={styles.kernelOwned} {...mark(MARKER.badge, badge.id)}>
      {badge.text}
    </ListText>
  );
}

/** A Cards row. The delete is a sibling of the row button, never a child: a `<button>` inside a `<button>` is dropped by every HTML parser. */
function cardRow(row: PanelRow, deps: DesktopPainterDeps): ReactNode {
  const open = control(row, 'open-card');
  const remove = control(row, 'delete-card');
  return (
    <li key={row.id} className={styles.cardItem} {...mark(MARKER.row, row.id)}>
      <button
        type="button"
        className={`${styles.cardRow} ${remove !== null ? styles.cardRowRemovable : ''}`}
        {...(open === null ? {} : { ...mark(MARKER.action, 'open-card'), ...wording(open) })}
        onClick={open === null ? undefined : () => deps.onOpenCard?.(open.id)}
      >
        <ListText tone="primary" className={styles.cardKind} {...mark(MARKER.field, FIELD.title)}>{row.title}</ListText>
        <span className={styles.cardMeta}>
          {row.activity !== null && <ActivityIndicator state={row.activity} spoken={activityLabelOf(row.activity)} />}
          {row.status !== null && <ListText tone="secondary" className={styles.cardStatus}
            {...mark(MARKER.status, row.status.token)} title={row.status.phrase}>{row.status.token}</ListText>}
          {row.kind !== null && (
            <ListText tone="secondary" className={styles.cardKindTag} {...mark(MARKER.field, FIELD.kind)}>{row.kind}</ListText>
          )}
          {row.badges.map(cardBadge)}
        </span>
      </button>
      {remove !== null && (
        <button
          type="button"
          data-nc-role="icon"
          className={styles.cardRemove}
          {...mark(MARKER.action, 'delete-card')}
          {...wording(remove)}
          onClick={() => deps.onDeleteCard?.(remove.id)}
        >
          <Icon name="close" size="sm" />
        </button>
      )}
    </li>
  );
}

/** A Task row: the reveal button always reveals the block; the kind is the worker-card affordance. A `<button>` may not nest inside a `<button>`, so the row is a plain `<li>`; "the row reveals the block" is a CSS sheet (`.taskReveal::before`), not DOM containment. */
function taskRow(row: PanelRow, deps: DesktopPainterDeps): ReactNode {
  const reveal = control(row, 'reveal-block');
  const open = control(row, 'open-card');
  const revealControl = (
    <button
      type="button"
      className={styles.taskReveal}
      {...(reveal === null ? {} : { ...mark(MARKER.action, 'reveal-block'), ...wording(reveal) })}
      onClick={reveal === null ? undefined : () => deps.onOpenTask?.(reveal.id)}
    >
      <ListText tone="primary" className={styles.taskKey} {...mark(MARKER.field, FIELD.title)}>{row.title}</ListText>
      {row.badges.map((badge) => (
        <ListText tone="secondary"
          key={badge.id}
          className={badge.struck ? styles.taskWithdrawn : styles.taskNote}
          {...mark(MARKER.badge, badge.id)}
        >{badge.text}</ListText>
      ))}
      {/* Spoken: the status word below is `aria-hidden` and names the run, not the verdict. */}
      {row.activity !== null && <ActivityIndicator state={row.activity} spoken={activityLabelOf(row.activity)} />}
      {row.status !== null && (
        <ListText tone="secondary"
          className={styles.taskStatusText}
          data-nc-task-status-text=""
          {...mark(MARKER.status, row.status.token)}
          aria-hidden="true"
          title={row.status.phrase}
        >{row.status.token}</ListText>
      )}
    </button>
  );
  return (
    <li key={row.id} className={styles.taskRow} {...mark(MARKER.row, row.id)}>
      {revealControl}
      {/* `title` describes the destination without touching the accessible name, which stays the visible word (WCAG 2.5.3). */}
      {row.kind !== null && (open === null
        ? <ListText tone="secondary" className={styles.taskKind} {...mark(MARKER.field, FIELD.kind)}>{row.kind}</ListText>
        : (
          <ListText as="button" tone="secondary"
            className={styles.taskKindButton}
            {...mark(MARKER.field, FIELD.kind)}
            {...mark(MARKER.action, 'open-card')}
            {...wording(open)}
            onClick={() => deps.onOpenCard?.(open.id)}
          >
            {row.kind}
          </ListText>
        ))}
    </li>
  );
}

/** The desktop panel's painter, rebuilt per render: `delete-card` support is `onDeleteCard !== undefined`, a fact about this render. `open-card` and `reveal-block` are drawn with or without a callback. */
export function makeDesktopPainter(deps: DesktopPainterDeps): RowPainter<DesktopLeaf> {
  const deleteSupport: ActionSupport = deps.onDeleteCard === undefined
    ? { supported: false, why: 'the host passed no onDeleteCard, so this render offers no delete' }
    : { supported: true };

  return {
    action: {
      'reveal-block': { supported: true },
      'open-card': { supported: true },
      'delete-card': deleteSupport,
    },

    row: (row) => ({
      slot: 'row',
      group: (key) => panelRowGroup(row, key),
      paint: (moduleKey) => (moduleKey === 'cards' ? cardRow(row, deps) : taskRow(row, deps)),
    }),

    empty: (text) => ({
      slot: 'empty',
      group: () => 'other',
      paint: () => <PanelEmpty key="empty" fieldMarker={FIELD.empty}>{text}</PanelEmpty>,
    }),

    module: (parts) => {
      const children = parts.children.map((leaf) => finish(leaf, parts.key));
      const rows = parts.children.every((leaf) => leaf.slot === 'row');
      const module = (
        <PanelModule
          key={parts.key}
          title={parts.title}
          action={parts.key === 'cards'
            ? deps.cardsAction
            : deps.taskSummary
              ? <ListText tone="count" className={styles.taskSummary} title={deps.taskSummary}>{deps.taskSummary}</ListText>
              : undefined}
          moduleMarker={parts.key}
          titleFieldMarker={FIELD.moduleTitle}
        >
          <div {...(parts.key === 'cards' ? { 'data-nc-card-inventory': '' } : { 'data-nc-task-inventory': '' })}>
          {!rows ? children : <InventoryGroups
            groups={inventorySections(parts.children, leaf => {
              if (leaf.slot === 'module') throw new Error('Nested inventory module');
              return leaf.group(parts.key);
            })}
            noun={parts.key === 'cards' ? 'card' : 'task'}
            renderRows={leaves => parts.key === 'cards'
              ? <ul className={styles.cards}>{leaves.map(leaf => finish(leaf, parts.key))}</ul>
              : <ul className={styles.tasks}>{leaves.map(leaf => finish(leaf, parts.key))}</ul>}
          />}
          </div>
        </PanelModule>
      );
      return {
        slot: 'module',
        node: parts.key === 'cards' && deps.afterCards !== undefined
          ? <Fragment key={parts.key}>{module}{deps.afterCards}</Fragment>
          : module,
      };
    },
  };
}

/** Resolve one of a module's children against the module it landed in. A module leaf here means the traversal changed shape, so it throws. */
function finish(leaf: DesktopLeaf, moduleKey: RowModuleView['key']): ReactNode {
  if (leaf.slot === 'module') {
    throw new Error(`paintModule handed the ${moduleKey} module a module leaf as a child`);
  }
  return leaf.paint(moduleKey);
}

/** `paintPanel`, unwrapped into nodes the page can render. It reads no key, so it cannot re-bind one to the view's order. */
export function paintDesktopPanel(
  painter: RowPainter<DesktopLeaf>,
  view: TrackPageView,
): readonly ReactNode[] {
  return paintPanel(painter, view).map((leaf) => {
    if (leaf.slot !== 'module') {
      throw new Error(`paintPanel returned a ${leaf.slot} leaf where a module was due`);
    }
    return leaf.node;
  });
}
