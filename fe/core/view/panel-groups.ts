import type { PanelRow, RowModuleView } from './panel.js';

export type InventoryGroupKey = 'working' | 'attention' | 'waiting' | 'failed' | 'done' | 'canceled' | 'ready' | 'other';
export type InventoryGroup<T> = Readonly<{ key: InventoryGroupKey; label: string; expanded: boolean; rows: readonly T[] }>;
const GROUPS = Object.freeze([
  Object.freeze({ key: 'working', label: 'In progress', expanded: true }),
  Object.freeze({ key: 'attention', label: 'Needs input', expanded: false }),
  Object.freeze({ key: 'waiting', label: 'Waiting', expanded: false }),
  Object.freeze({ key: 'failed', label: 'Failed', expanded: false }),
  Object.freeze({ key: 'done', label: 'Completed', expanded: false }),
  Object.freeze({ key: 'canceled', label: 'Canceled', expanded: false }),
  Object.freeze({ key: 'ready', label: 'Available', expanded: false }),
  Object.freeze({ key: 'other', label: 'Other', expanded: false }),
] as const);

export function panelRowGroup(row: PanelRow, kind: RowModuleView['key']): InventoryGroupKey {
  if (row.badges.some(badge => badge.struck)) return 'canceled';
  switch (row.status?.token ?? null) {
    case 'running': case 'working': case 'starting': case 'turn_pending': case 'dispatched': case 'verifying': return 'working';
    case 'needs_input': case 'awaiting_input': case 'blocked': return 'attention';
    case 'pending': case 'queued': case 'waiting': case 'ready': case 'not-ready': case 'awaiting_refresh': case 'awaiting_projection': return 'waiting';
    case 'done': case 'completed': case 'succeeded': case 'exited': return 'done';
    case 'failed': case 'errored': return 'failed';
    case 'canceled': case 'cancelled': case 'superseded': case 'withdrawn': return 'canceled';
    case 'idle': case null: return kind === 'cards' ? 'ready' : 'waiting';
    default: return 'other';
  }
}

export function groupInventory<T>(rows: readonly T[], groupOf: (row: T) => InventoryGroupKey): readonly InventoryGroup<T>[] {
  return GROUPS.map(group => ({ ...group, rows: rows.filter(row => groupOf(row) === group.key) }))
    .filter(group => group.rows.length > 0);
}

export function groupPanelRows(rows: readonly PanelRow[], kind: RowModuleView['key']): readonly InventoryGroup<PanelRow>[] {
  return groupInventory(rows, row => panelRowGroup(row, kind));
}

/** Painters preserve the supplied projection order; the derivation owns sorting. */
export function inventorySections<T>(rows: readonly T[], groupOf: (row: T) => InventoryGroupKey): readonly InventoryGroup<T>[] {
  const sections: { key: InventoryGroupKey; label: string; expanded: boolean; rows: T[] }[] = [];
  for (const row of rows) {
    const key = groupOf(row);
    const previous = sections.at(-1);
    if (previous?.key === key) previous.rows.push(row);
    else {
      const descriptor = GROUPS.find(group => group.key === key);
      if (descriptor === undefined) throw new Error(`Unknown inventory group: ${key}`);
      sections.push({ ...descriptor, rows: [row] });
    }
  }
  return sections;
}
