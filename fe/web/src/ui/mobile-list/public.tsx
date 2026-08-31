import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import type { ReactNode } from 'react';

import { MobileHeader } from '../mobile-header/public.tsx';
import styles from './mobile-list.module.css';

function motionClass(motion: 'none' | 'forward' | 'back'): string {
  if (motion === 'forward') return styles.pageForward;
  if (motion === 'back') return styles.pageBack;
  return '';
}

export function MobileListPage({ title, backLabel, onBack, motion = 'none', children }: Readonly<{
  title: string;
  backLabel?: string;
  onBack?: () => void;
  motion?: 'none' | 'forward' | 'back';
  children: ReactNode;
}>) {
  return (
    <div className={`${styles.page} ${motionClass(motion)}`}>
      <MobileHeader title={title} backLabel={backLabel} onBack={onBack} />
      <div className={styles.content}>{children}</div>
    </div>
  );
}

export function MobileList({ title, children }: Readonly<{ title?: string; children: ReactNode }>) {
  return (
    <section className={styles.section}>
      {title !== undefined && <h3>{title}</h3>}
      <AstryxList className={styles.list} density="spacious">{children}</AstryxList>
    </section>
  );
}

export function MobileListEmpty({ children }: Readonly<{ children: ReactNode }>) {
  return <li><p className={styles.empty}>{children}</p></li>;
}

export function MobileListItem({
  title, meta, startContent, ariaLabel, nested = false, titleVariant = 'interface', onSelect,
}: Readonly<{
  title: string;
  meta?: ReactNode;
  startContent?: ReactNode;
  ariaLabel?: string;
  /** Visually nests a second-level row while keeping one flat, touch-friendly list. */
  nested?: boolean;
  /** Document titles use the report's editorial typography; utility rows stay sans. */
  titleVariant?: 'interface' | 'document';
  onSelect: () => void;
}>) {
  const metaLabel = typeof meta === 'string' || typeof meta === 'number' ? String(meta) : null;
  return (
    <AstryxListItem
      className={`${styles.item} ${nested ? styles.itemNested : ''}`}
      label={(
        <span className={`${styles.itemTitle} ${titleVariant === 'document' ? styles.itemTitleDocument : ''}`}>
          {title}
        </span>
      )}
      startContent={startContent}
      onClick={() => onSelect()}
      aria-label={ariaLabel ?? (metaLabel === null ? undefined : `${title}, ${metaLabel}`)}
      endContent={meta === undefined ? undefined : <span className={styles.meta}>{meta}</span>}
    />
  );
}
