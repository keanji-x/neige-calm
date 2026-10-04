// Browser-independent sidebar presentation order. Membership stays with each owner.
export type SidebarSectionId = 'waiting' | 'pinned' | 'unread' | 'running' | 'areas';
export type SidebarGroupId = SidebarSectionId | `area:${string}`;
export type SidebarOrderScope = 'sections' | 'areas';
export type SidebarMove = 'up' | 'down';

export const SIDEBAR_SECTION_IDS: readonly SidebarSectionId[] = Object.freeze(['waiting', 'pinned', 'unread', 'running', 'areas']);

/** Retain saved positions, drop stale/duplicate IDs and append newly registered groups. */
export function sidebarOrder<T extends string>(available: readonly T[], saved: unknown): T[] {
  const ids = new Set(available);
  const ordered: T[] = [];
  if (Array.isArray(saved) && saved.every((id) => typeof id === 'string')) {
    for (const id of saved) {
      if (ids.delete(id as T)) ordered.push(id as T);
    }
  }
  for (const id of available) if (ids.delete(id)) ordered.push(id);
  return ordered;
}

/** Swap visible siblings only; hidden groups retain their slots in the full order. */
export function moveSidebarGroup<T extends string>(order: readonly T[], visible: readonly T[], id: T, direction: SidebarMove): T[] {
  const index = visible.indexOf(id);
  const neighbor = visible[index + (direction === 'up' ? -1 : 1)];
  const from = order.indexOf(id);
  const to = neighbor === undefined ? -1 : order.indexOf(neighbor);
  const next = [...order];
  if (index < 0 || from < 0 || to < 0) return next;
  [next[from], next[to]] = [next[to], next[from]];
  return next;
}
