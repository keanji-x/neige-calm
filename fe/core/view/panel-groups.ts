import { agentProviderSchema } from '../api/schemas.js';
import type { PanelRow, RowModuleView } from './panel.js';

export type InventoryGroupKey = 'terminals' | 'agents' | 'other-tools' | 'working' | 'attention' | 'waiting' | 'failed' | 'done' | 'canceled' | 'other';
export type InventoryGroup<T> = Readonly<{ key: InventoryGroupKey; label: string; expanded: boolean; attentionCount: number; rows: readonly T[] }>;
const GROUPS = Object.freeze([
  Object.freeze({ key: 'terminals', label: 'Terminals', expanded: false }),
  Object.freeze({ key: 'agents', label: 'Agents', expanded: false }),
  Object.freeze({ key: 'other-tools', label: 'Other tools', expanded: false }),
  Object.freeze({ key: 'working', label: 'In progress', expanded: true }),
  Object.freeze({ key: 'attention', label: 'Needs input', expanded: true }),
  Object.freeze({ key: 'waiting', label: 'Waiting', expanded: false }),
  Object.freeze({ key: 'failed', label: 'Failed', expanded: true }),
  Object.freeze({ key: 'done', label: 'Completed', expanded: false }),
  Object.freeze({ key: 'canceled', label: 'Canceled', expanded: false }),
  Object.freeze({ key: 'other', label: 'Other', expanded: false }),
] as const);

export function panelRowGroup(row: PanelRow, kind: RowModuleView['key']): InventoryGroupKey {
  if (kind === 'cards') {
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
  if (kind !== 'cards') return false;
  if (row.badges.some(badge => badge.struck)) return false;
  return row.activity === 'failed' || row.activity === 'attention'
    || (row.status !== null && ['failed', 'errored', 'needs_input', 'awaiting_input', 'blocked'].includes(row.status.token));
}

/** One descriptor/attention resolver serves sorted inventories and order-preserving sections. */
function inventoryGroup<T>(key: InventoryGroupKey, rows: readonly T[], attentionOf: (row: T) => boolean): InventoryGroup<T> {
  const descriptor = GROUPS.find(group => group.key === key);
  if (descriptor === undefined) throw new Error(`Unknown inventory group: ${key}`);
  const attentionCount = rows.filter(attentionOf).length;
  return { ...descriptor, rows, attentionCount, expanded: descriptor.expanded || attentionCount > 0 };
}

export function groupInventory<T>(rows: readonly T[], groupOf: (row: T) => InventoryGroupKey,
  attentionOf: (row: T) => boolean): readonly InventoryGroup<T>[] {
  return GROUPS.map(group => inventoryGroup(group.key, rows.filter(row => groupOf(row) === group.key), attentionOf))
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
