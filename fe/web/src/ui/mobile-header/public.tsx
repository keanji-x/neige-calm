import { floatingControlClassName, GuideAnchor } from '../floating-control/public.ts';
import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { Icon } from '../icon/public.tsx';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import type { ReactNode, RefCallback } from 'react';

import styles from './mobile-header.module.css';

/** Marked module titles remain pure headings. Interactive app title content
 * cannot consume the projection marker that belongs to a module title. */
type MobileHeaderTitle =
  | Readonly<{ titleContent?: never; titleFieldMarker?: string; titleText?: never }>
  | Readonly<{ titleContent?: never; titleFieldMarker?: never; titleText: ReactNode }>
  | Readonly<{ titleContent: ReactNode; titleFieldMarker?: never; titleText?: never }>;

export function MobileHeader({
  title, meta, level = 2, backLabel, backGuideId, onBack, actions, actionsHidden = false, titleFieldMarker, titleContent, titleText, leading, className,
}: Readonly<{
  title: string;
  className?: string;
  meta?: ReactNode;
  level?: 1 | 2;
  backLabel?: string;
  /** The host may declare its existing Back control as a guide location. */
  backGuideId?: string;
  onBack?: () => void;
  actions?: ReactNode;
  /** Keeps a declared action host mounted without reserving a visible column. */
  actionsHidden?: boolean;
  /** Replaces the standard Back control only at a root navigation entry. */
  leading?: ReactNode;
}> & MobileHeaderTitle) {
  const back = (anchor?: RefCallback<HTMLElement>, describedBy?: string) => <AstryxIconButton
    ref={anchor} aria-describedby={describedBy}
    className={`${styles.back} ${floatingControlClassName}`}
    label={`Back to ${backLabel ?? 'previous page'}`}
    variant="ghost"
    size="lg"
    icon={<Icon name="chevron-left" />}
    onClick={onBack}
  />;
  return (
    <header
      className={[styles.header, className].filter(Boolean).join(' ')}
      data-nc-mobile-header=""
    >
      <span className={styles.leading}>
        {leading ?? (onBack !== undefined && (
          backGuideId === undefined ? back() : <GuideAnchor id={backGuideId}>{back}</GuideAnchor>
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
          {titleText ?? title}
        </AstryxHeading>}
        {meta}
      </div>
      <span className={styles.trailing} hidden={actionsHidden}>{actions}</span>
    </header>
  );
}
