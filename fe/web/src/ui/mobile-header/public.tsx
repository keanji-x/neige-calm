import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import type { ReactNode } from 'react';

import styles from './mobile-header.module.css';

export function MobileHeader({ title, level = 2, backLabel, onBack, actions }: Readonly<{
  title: string;
  level?: 1 | 2;
  backLabel?: string;
  onBack?: () => void;
  actions?: ReactNode;
}>) {
  return (
    <header className={styles.header} data-nc-mobile-header="">
      <span className={styles.leading}>
        {onBack !== undefined && (
          <AstryxIconButton
            className={styles.back}
            label={`Back to ${backLabel ?? 'previous page'}`}
            variant="ghost"
            size="lg"
            icon={<AstryxIcon icon="chevronLeft" size="md" color="inherit" />}
            onClick={onBack}
          />
        )}
      </span>
      <AstryxHeading level={level} color="secondary" maxLines={1} className={styles.title}>
        {title}
      </AstryxHeading>
      <span className={styles.trailing}>{actions}</span>
    </header>
  );
}
