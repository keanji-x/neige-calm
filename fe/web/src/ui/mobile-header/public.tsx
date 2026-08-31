import type { ReactNode } from 'react';

import { Icon } from '../icon/public.tsx';
import styles from './mobile-header.module.css';

export function MobileHeader({ title, level = 2, backLabel, onBack, actions }: Readonly<{
  title: string;
  level?: 1 | 2;
  backLabel?: string;
  onBack?: () => void;
  actions?: ReactNode;
}>) {
  const heading = level === 1
    ? <h1 className={styles.title}>{title}</h1>
    : <h2 className={styles.title}>{title}</h2>;
  return (
    <header className={styles.header} data-nc-mobile-header="">
      <span className={styles.leading}>
        {onBack !== undefined && (
          <button type="button" className={styles.back} aria-label={`Back to ${backLabel ?? 'previous page'}`} onClick={onBack}>
            <Icon name="arrow-left" />
          </button>
        )}
      </span>
      {heading}
      <span className={styles.trailing}>{actions}</span>
    </header>
  );
}
