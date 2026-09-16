import { ListText } from '../../../ui/list-typography/public.tsx';
import type { CSSProperties, ReactNode } from 'react';
import type { InventoryGroup } from '../../../../../core/view/panel-groups.ts';
import { useState } from '../../../ui/state/public.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './inventory-groups.module.css';

/** Keep collapsed rows mounted so actions, live state and the projection remain intact. */
export function InventoryGroups<T>({ groups, noun, renderRows }: Readonly<{
  groups: readonly InventoryGroup<T>[];
  noun: 'task' | 'card';
  renderRows: (rows: readonly T[]) => ReactNode;
}>) {
  const [opened, setOpened] = useState<Readonly<Record<string, boolean>>>({});
  const sections = groups.map((group, index) => {
    const id = `${group.key}-${groups.slice(0, index).filter(previous => previous.key === group.key).length}`;
    return { ...group, id, open: opened[id] ?? group.expanded };
  });
  const budget = {
    '--nc-inventory-group-count': sections.length,
    '--nc-inventory-open-count': Math.max(1, sections.filter(group => group.open).length),
  } as CSSProperties;
  return <div className={styles.groups} style={budget}>
    {sections.map(group => <details key={group.id} className={styles.group} open={group.open}
      onToggle={event => {
        const open = event.currentTarget.open;
        setOpened(previous => previous[group.id] === open ? previous : { ...previous, [group.id]: open });
      }} data-nc-inventory-group={group.key}>
      <summary className={styles.summary} aria-label={`${group.label}, ${group.rows.length} ${noun}${group.rows.length === 1 ? '' : 's'}`}>
        <ListText tone="group" className={styles.label}>{group.label}</ListText>
        <ListText tone="count" aria-hidden="true">{group.rows.length}</ListText>
        <span className={styles.chevron} aria-hidden="true"><Icon name="chevron-right" size="sm" /></span>
      </summary>
      <div className={styles.content}>{renderRows(group.rows)}</div>
    </details>)}
  </div>;
}
