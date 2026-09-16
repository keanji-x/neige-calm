import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import type { ReactNode } from 'react';

import styles from './mobile-header.module.css';

/** Marked module titles remain pure headings. Interactive app title content
 * cannot consume the projection marker that belongs to a module title. */
type MobileHeaderTitle =
  | Readonly<{ titleContent?: never; titleFieldMarker?: string }>
  | Readonly<{ titleContent: ReactNode; titleFieldMarker?: never }>;

export function MobileHeader({
  title, meta, level = 2, backLabel, onBack, actions, titleFieldMarker, titleContent, leading,
}: Readonly<{
  title: string;
  meta?: ReactNode;
  level?: 1 | 2;
  backLabel?: string;
  onBack?: () => void;
  actions?: ReactNode;
  /** Replaces the standard Back control only at a root navigation entry. */
  leading?: ReactNode;
}> & MobileHeaderTitle) {
  return (
    <header
      className={styles.header}
      data-nc-mobile-header=""
    >
      <span className={styles.leading}>
        {leading ?? (onBack !== undefined && (
          <AstryxIconButton
            className={styles.back}
            label={`Back to ${backLabel ?? 'previous page'}`}
            variant="ghost"
            size="lg"
            icon={<AstryxIcon icon="chevronLeft" size="md" color="inherit" />}
            onClick={onBack}
          />
        ))}
      </span>
      <div className={styles.titleGroup}>
        {titleContent ?? <AstryxHeading
          level={level}
          color="secondary"
          maxLines={1}
          className={styles.title}
          {...(titleFieldMarker === undefined ? {} : { 'data-nc-field': titleFieldMarker })}
        >
          {title}
        </AstryxHeading>}
        {meta}
      </div>
      <span className={styles.trailing}>{actions}</span>
    </header>
  );
}
