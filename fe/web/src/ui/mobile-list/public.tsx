import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import type { ReactNode } from 'react';

import { MobileHeader } from '../mobile-header/public.tsx';
import styles from './mobile-list.module.css';

function motionClass(motion: 'none' | 'forward' | 'back'): string {
  if (motion === 'forward') return styles.pageForward;
  if (motion === 'back') return styles.pageBack;
  return '';
}

export function MobileListPage({ title, backLabel, onBack, note, motion = 'none', children }: Readonly<{
  title: string;
  backLabel?: string;
  onBack?: () => void;
  note?: ReactNode;
  motion?: 'none' | 'forward' | 'back';
  children: ReactNode;
}>) {
  return (
    <div className={`${styles.page} ${motionClass(motion)}`}>
      <MobileHeader title={title} backLabel={backLabel} onBack={onBack} />
      {note !== undefined && <div className={styles.note}>{note}</div>}
      <div className={styles.content}>{children}</div>
    </div>
  );
}

export function MobileList({ title, children }: Readonly<{ title?: string; children: ReactNode }>) {
  return (
    <section className={styles.section}>
      {title !== undefined && <h3>{title}</h3>}
      <AstryxList className={styles.list} density="spacious" hasDividers>{children}</AstryxList>
    </section>
  );
}

export function MobileListEmpty({ children }: Readonly<{ children: ReactNode }>) {
  return <li><p className={styles.empty}>{children}</p></li>;
}

export function MobileListItem({ title, description, meta, ariaLabel, nested = false, onSelect }: Readonly<{
  title: string;
  description?: ReactNode;
  meta?: ReactNode;
  ariaLabel?: string;
  /** Visually nests a second-level row while keeping one flat, touch-friendly list. */
  nested?: boolean;
  onSelect: () => void;
}>) {
  return (
    <AstryxListItem
      className={`${styles.item} ${nested ? styles.itemNested : ''}`}
      label={title}
      description={description}
      onClick={() => onSelect()}
      aria-label={ariaLabel}
      endContent={(
        <span className={styles.endContent}>
          {meta !== undefined && <span className={styles.meta}>{meta}</span>}
          <AstryxIcon icon="chevronRight" size="sm" color="secondary" />
        </span>
      )}
    />
  );
}
