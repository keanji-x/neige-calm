// The mobile track panel, painted from `core/view`'s panel view model.
// Mobile does not call `paintPanel`: it drills into one module at a time, so each page calls `paintModule` once through `paintMobileModule`.

import { inventorySections, panelRowGroup, type InventoryGroupKey } from '../../../../../core/view/panel-groups.ts';
import { InventoryGroups } from './inventory-groups.tsx';
import type { ReactNode } from 'react';

import { FIELD, MARKER, paintModule } from '../../../../../core/view/panel.ts';
import type {
  PanelRow, RowAction, RowBadge, RowModuleView, RowPainter, RowStatus,
} from '../../../../../core/view/panel.ts';
import { activityLabelOf } from '../../../../../core/domain/activity.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../../ui/mobile-list/public.tsx';
import styles from './page.module.css';

/** A row or an empty line, painted as far as it can be here: only `module()` knows which module it lands in. */
type PendingLeaf = Readonly<{
  slot: 'row' | 'empty';
  group: (moduleKey: RowModuleView['key']) => InventoryGroupKey;
  paint: (moduleKey: RowModuleView['key']) => ReactNode;
}>;

/** A module, which needs nothing further: `module()` was handed `parts.key`. */
type ModuleLeaf = Readonly<{ slot: 'module'; node: ReactNode }>;

export type MobileLeaf = PendingLeaf | ModuleLeaf;

export type MobilePainterDeps = Readonly<{
  /** Reveal a task's block in the document: the one action this surface supports, hosted on the row root. */
  onOpenTask?: (blockId: string) => void;
  /** The mobile page chrome: what the back button says and where it goes. Not view-model content, so it travels in the factory's closure. */
  backLabel?: string;
  onBack?: () => void;
}>;

/** One marker attribute, spelled from `MARKER` / `FIELD` so no name is retyped here. */
const mark = (name: string, value: string): Readonly<Record<string, string>> => ({ [name]: value });

function cardBadge(badge: RowBadge): ReactNode {
  return (
    <span key={badge.id} {...mark(MARKER.badge, badge.id)}>{badge.text}</span>
  );
}

/** A Task row's declaration badge. `struck` has no carrier in the projection framework, so it is drawn here and pinned by a class assertion. */
function taskBadge(badge: RowBadge): ReactNode {
  return (
    <span
      key={badge.id}
      className={badge.struck ? styles.mobileRowStruck : undefined}
      {...mark(MARKER.badge, badge.id)}
    >{badge.text}</span>
  );
}

/** The status, in the meta lane: `data-nc-status` holds the bare token and `title` the phrase. Astryx lays this out as a sibling of the invisible button, so `taskRow` also hands the phrase to the row's accessible description. A pending reason owns the row root's title, so the status word then omits its own. */
function statusWord(status: RowStatus, ownsTitle: boolean): ReactNode {
  return (
    <span
      key="status"
      {...mark(MARKER.status, status.token)}
      {...(ownsTitle ? { title: status.phrase } : {})}
    >
      {status.token}
    </span>
  );
}

/** The `reveal-block` action, or null when the view model offered none. All four fields come back, `label` included: the derivation words it `null` today, but the channel is the view model's to fill. */
function reveal(
  row: PanelRow,
): Readonly<{
  blockId: string;
  label: string | null;
  hint: string | null;
  description: string | null;
}> | null {
  for (const action of row.actions) {
    if (action.kind !== 'reveal-block') continue;
    return {
      blockId: action.blockId,
      label: action.label,
      hint: action.hint,
      description: action.description,
    };
  }
  return null;
}

/** A Tasks row. The row root is both the row carrier and the action host. `label` becomes `aria-label` only when offered — a fabricated second accessible name would override the visible text (WCAG 2.5.3); `hint` goes to the `<li>`'s `title`, `description` to the generated button. */
function taskRow(row: PanelRow, deps: MobilePainterDeps): ReactNode {
  const action = reveal(row);
  const accessibleDescription = action?.description ?? null;
  const meta: readonly ReactNode[] = [
    ...row.badges.map(taskBadge),
    /* The same indicator the desktop row paints, spoken for the same reason: no control here names the verdict. */
    ...(row.activity === null ? [] : [
      <ActivityIndicator key="activity" state={row.activity} spoken={activityLabelOf(row.activity)} />,
    ]),
    ...(row.status === null ? [] : [statusWord(
      row.status,
      row.status.token !== 'pending' || action === null || action.hint === null,
    )]),
    ...(row.kind === null
      ? []
      : [<span key="kind" {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>]),
  ];
  return (
    <MobileListItem
      key={row.id}
      title={row.title}
      rowMarker={row.id}
      titleFieldMarker={FIELD.title}
      {...(accessibleDescription === null ? {} : { accessibleDescription })}
      {...(action === null ? {} : {
        rowActionMarker: 'reveal-block' satisfies RowAction['kind'],
        onSelect: () => deps.onOpenTask?.(action.blockId),
        ...(action.label === null ? {} : { ariaLabel: action.label }),
        ...(action.hint === null ? {} : { hint: action.hint }),
      })}
      meta={meta.length === 0 ? undefined : <span className={styles.mobileRowMeta}>{meta}</span>}
    />
  );
}

/** A Cards row: not a control — both card actions are unsupported, so no `onSelect` is passed and no invisible button is generated. The kind and the badges are separate elements in the meta lane, each its own leaf carrier. */
function cardRow(row: PanelRow): ReactNode {
  const meta: readonly ReactNode[] = [
    ...(row.kind === null
      ? []
      : [<span key="kind" {...mark(MARKER.field, FIELD.kind)}>{row.kind}</span>]),
    ...row.badges.map(cardBadge),
    /* Spoken, as on the desktop card row: the status word is a different fact. */
    ...(row.activity === null ? [] : [
      <ActivityIndicator key="activity" state={row.activity} spoken={activityLabelOf(row.activity)} />,
    ]),
    ...(row.status === null ? [] : [statusWord(row.status, true)]),
  ];
  return (
    <MobileListItem
      key={row.id}
      title={row.title}
      rowMarker={row.id}
      titleFieldMarker={FIELD.title}
      meta={meta.length === 0 ? undefined : <span className={styles.mobileRowMeta}>{meta}</span>}
    />
  );
}

/** The mobile panel's painter, rebuilt per render. No entry is bound to a host prop: not offering the card actions is a fact about this viewport. */
export function makeMobilePainter(deps: MobilePainterDeps): RowPainter<MobileLeaf> {
  return {
    action: {
      'reveal-block': { supported: true },
      'open-card': {
        supported: false,
        why: 'cards render poorly at this viewport, so opening a card is not offered on mobile (owner, #1234)',
      },
      'delete-card': {
        supported: false,
        why: 'the same call, kept simple: card operations are not offered on mobile (owner, #1234)',
      },
    },

    /* Dispatch on the module, never on the row's shape. A third module key throws rather than falling back to a Cards row. */
    row: (row) => ({
      slot: 'row',
      group: (key) => panelRowGroup(row, key),
      paint: (moduleKey) => {
        if (moduleKey === 'cards') return cardRow(row);
        if (moduleKey === 'tasks') return taskRow(row, deps);
        const unknown: never = moduleKey;
        throw new Error(`the mobile painter has no ${String(unknown)} row`);
      },
    }),

    empty: (text) => ({
      slot: 'empty',
      group: () => 'other',
      paint: () => <MobileListEmpty key="empty" fieldMarker={FIELD.empty}>{text}</MobileListEmpty>,
    }),

    module: (parts) => {
      const leaves = parts.children.map(leaf => ({ leaf, node: finish(leaf, parts.key) }));
      return {
      slot: 'module',
      node: (
        <MobileListPage
          key={parts.key}
          title={parts.title}
          backLabel={deps.backLabel}
          onBack={deps.onBack}
          moduleMarker={parts.key}
          titleFieldMarker={FIELD.moduleTitle}
        >
          {parts.children.some(leaf => leaf.slot !== 'row')
            ? <MobileList>{parts.children.map(leaf => finish(leaf, parts.key))}</MobileList>
            : <InventoryGroups
              groups={inventorySections(leaves, ({ leaf }) => {
                if (leaf.slot === 'module') throw new Error('Nested inventory module');
                return leaf.group(parts.key);
              })}
              noun={parts.key === 'cards' ? 'card' : 'task'}
              renderRows={rows => <MobileList>{rows.map(row => row.node)}</MobileList>}
            />}

        </MobileListPage>
      ),
    };
    },
  };
}

/** Resolve one of a module's children against the module it landed in. A module leaf here means the traversal changed shape, so it throws. */
function finish(leaf: MobileLeaf, moduleKey: RowModuleView['key']): ReactNode {
  if (leaf.slot === 'module') {
    throw new Error(`paintModule handed the ${moduleKey} module a module leaf as a child`);
  }
  return leaf.paint(moduleKey);
}

/** `paintModule`, unwrapped into the one page the mobile surface shows: the caller picks the module the reader drilled into. */
export function paintMobileModule(
  painter: RowPainter<MobileLeaf>,
  module: RowModuleView,
): ReactNode {
  const leaf = paintModule(painter, module);
  if (leaf.slot !== 'module') {
    throw new Error(`paintModule returned a ${leaf.slot} leaf where a module was due`);
  }
  return leaf.node;
}
