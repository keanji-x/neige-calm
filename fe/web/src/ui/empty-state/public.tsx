import type { ReactElement } from 'react';
import styles from './empty-state.module.css';

/** Three required content fields. Empty states have no actions or extra slots. */
export type EmptyStateSchema = Readonly<{
  icon: ReactElement;
  title: string;
  description: string;
}>;

export function EmptyState({ title, description, icon, variant = 'page', align = 'start', titleFieldMarker }: EmptyStateSchema & Readonly<{
  variant?: 'page' | 'panel' | 'compact';
  titleFieldMarker?: string;
  align?: 'start' | 'center';
}>) {
  const Heading = variant === 'page' ? 'h2' : 'h3';
  return <div className={`${styles.empty} ${variant === 'panel' ? styles.panel : variant === 'compact' ? styles.compact : ''} ${align === 'center' ? styles.center : ''}`}>
    <div className={styles.heading}>
      <div className={styles.visual} aria-hidden="true">{icon}</div>
      <Heading className={styles.title} {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}>{title}</Heading>
    </div>
    <p className={styles.description}>{description}</p>
  </div>;
}
