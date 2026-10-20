import { agentProviderSchema } from '../api/schemas.js';
import type { PanelRow, RowModuleView } from './panel.js';

export type InventoryGroupKey = keyof typeof GROUPS;
export type InventoryGroup<T> = Readonly<{ key: InventoryGroupKey; label: string; expanded: boolean; attentionCount: number; rows: readonly T[] }>;
const GROUPS = Object.freeze({
  'terminals': Object.freeze({ label: 'Terminals', expanded: false }),
  'agents': Object.freeze({ label: 'Agents', expanded: false }),
  'other-tools': Object.freeze({ label: 'Other tools', expanded: false }),
  'working': Object.freeze({ label: 'In progress', expanded: true }),
  'attention': Object.freeze({ label: 'Needs input', expanded: true }),
  'waiting': Object.freeze({ label: 'Waiting', expanded: false }),
  'failed': Object.freeze({ label: 'Failed', expanded: true }),
  'done': Object.freeze({ label: 'Completed', expanded: false }),
  'canceled': Object.freeze({ label: 'Canceled', expanded: false }),
  'other': Object.freeze({ label: 'Other', expanded: false }),
} as const);

export function panelRowGroup(row: PanelRow, kind: RowModuleView['key']): InventoryGroupKey {
  if (kind === 'cards' || kind === 'temporary') {
    if (row.kind === 'terminal') return 'terminals';
    return agentProviderSchema.safeParse(row.kind).success ? 'agents' : 'other-tools';
  }
  if (row.badges.some(badge => badge.struck)) return 'canceled';
  switch (row.status?.token ?? null) {
    case 'running': case 'working': case 'starting': case 'turn_pending': case 'dispatched': case 'verifying': return 'working';
    case 'needs_input': case 'awaiting_input': case 'blocked': return 'attention';
    case 'pending': case 'queued': case 'waiting': case 'ready': case 'not-ready': case 'awaiting_projection': return 'waiting';
    case 'done': case 'completed': case 'succeeded': case 'exited': return 'done';
    case 'failed': case 'errored': return 'failed';
    case 'canceled': case 'cancelled': case 'superseded': case 'withdrawn': return 'canceled';
    case 'idle': case null: return 'waiting';
    default: return 'other';
  }
}

/** Explicit kernel activity and runtime evidence make a tool group important; no inferred policy. */
export function inventoryNeedsAttention(row: PanelRow, kind: RowModuleView['key']): boolean {
  if (kind !== 'cards' && kind !== 'temporary') return false;
  if (row.badges.some(badge => badge.struck)) return false;
  return row.activity === 'failed' || row.activity === 'attention'
    || (row.status !== null && ['failed', 'errored', 'needs_input', 'awaiting_input', 'blocked'].includes(row.status.token));
}

/** One descriptor/attention resolver serves sorted inventories and order-preserving sections. */
function inventoryGroup<T>(key: InventoryGroupKey, rows: readonly T[], attentionOf: (row: T) => boolean): InventoryGroup<T> {
  const descriptor = GROUPS[key];
  const attentionCount = rows.filter(attentionOf).length;
  return { key, ...descriptor, rows, attentionCount, expanded: descriptor.expanded || attentionCount > 0 };
}

export function groupInventory<T>(rows: readonly T[], groupOf: (row: T) => InventoryGroupKey,
  attentionOf: (row: T) => boolean): readonly InventoryGroup<T>[] {
  return (Object.keys(GROUPS) as InventoryGroupKey[]).map(key => inventoryGroup(key, rows.filter(row => groupOf(row) === key), attentionOf))
    .filter(group => group.rows.length > 0);
}

export function groupPanelRows(rows: readonly PanelRow[], kind: RowModuleView['key']): readonly InventoryGroup<PanelRow>[] {
  return groupInventory(rows, row => panelRowGroup(row, kind), row => inventoryNeedsAttention(row, kind));
}

/** Painters preserve the supplied projection order; the derivation owns sorting and attention. */
export function inventorySections<T>(rows: readonly T[], groupOf: (row: T) => InventoryGroupKey,
  attentionOf: (row: T) => boolean): readonly InventoryGroup<T>[] {
  const sections: { key: InventoryGroupKey; rows: T[] }[] = [];
  for (const row of rows) {
    const key = groupOf(row);
    const previous = sections.at(-1);
    if (previous?.key === key) previous.rows.push(row);
    else sections.push({ key, rows: [row] });
  }
  return sections.map(section => inventoryGroup(section.key, section.rows, attentionOf));
}

/** Finished work can be tucked away without hiding failures or requests for input. */
export function isFinishedTaskGroup(group: InventoryGroupKey): boolean {
  return group === 'done' || group === 'canceled';
}

/** Task lifecycle, not Worker attention, determines whether a Progress entry has ended. */
export function progressRowFinished(row: PanelRow): boolean {
  const state = panelRowGroup(row, 'tasks');
  return isFinishedTaskGroup(state) || state === 'failed';
}
