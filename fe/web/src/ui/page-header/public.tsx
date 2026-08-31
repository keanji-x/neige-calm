import type { ReactNode, RefObject } from 'react';

import { Icon } from '../icon/public.tsx';
import styles from './page-header.module.css';

export type PageHeaderProps = Readonly<{
  breadcrumb?: ReactNode;
  title: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  identity?: ReactNode;
  align?: 'page' | 'document';
}>;

export function PageHeader({ breadcrumb, title, meta, actions, identity, align = 'page' }: PageHeaderProps) {
  const rows = 1 + (breadcrumb === undefined ? 0 : 1) + (identity === undefined ? 0 : 1);
  return (
    <header
      className={styles.header}
      data-nc-header-rows={String(rows) as '1' | '2' | '3'}
      data-nc-align={align}
    >
      {breadcrumb !== undefined && <div className={styles.crumbRow}>{breadcrumb}</div>}
      <div className={styles.titleRow}>
        {title}
        {meta}
        <span className={styles.spring} />
        {actions}
      </div>
      {identity !== undefined && <div className={styles.identityRow}>{identity}</div>}
    </header>
  );
}

export function PageTitle({ children, titleRef }: {
  children: ReactNode;
  titleRef?: RefObject<HTMLHeadingElement | null>;
}) {
  return (
    <h1 ref={titleRef} className={styles.title} data-nc-page-title tabIndex={-1}>
      {children}
    </h1>
  );
}

export function Breadcrumb({ ancestor, current, onNavigate, onNavigateCurrent, backLabel, onBack }: {
  ancestor: string;
  current?: ReactNode;
  onNavigate: () => void;
  onNavigateCurrent?: () => void;
  backLabel?: string;
  onBack?: () => void;
}) {
  return (
    <nav className={styles.crumbs} aria-label="Breadcrumb">
      {onBack !== undefined && (
        <button type="button" data-nc-role="icon" className={styles.back}
          aria-label={backLabel ?? 'Back'} onClick={onBack}><Icon name="arrow-left" /></button>
      )}
      <button type="button" data-nc-role="row" className={styles.crumbLink} onClick={onNavigate}>
        {ancestor}
      </button>
      {current !== undefined && (
        <>
          <span className={styles.crumbSep} aria-hidden="true">/</span>
          {onNavigateCurrent === undefined
            ? <span className={styles.crumbCurrent}>{current}</span>
            : (
              <button type="button" data-nc-role="row" className={`${styles.crumbLink} ${styles.crumbCurrent}`} onClick={onNavigateCurrent}>
                {current}
              </button>
            )}
        </>
      )}
    </nav>
  );
}
