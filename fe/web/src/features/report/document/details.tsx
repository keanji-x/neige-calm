// Shared report appendix disclosure, also used by the report's Reference section.
import type { ReactNode } from 'react';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './document.module.css';

export function ReportDetails({ title, meta, layout, variant = 'section', reference = false, onToggle, children }: Readonly<{
  title: string; meta?: ReactNode; layout: 'grid' | 'appendix'; variant?: 'section' | 'entry';
  reference?: boolean; onToggle?: (open: boolean) => void; children: ReactNode;
}>) {
  const Heading = variant === 'section' ? 'h2' : 'h3';
  return <details className={`${styles.reference} ${layout === 'appendix' ? styles.appendix : ''} ${variant === 'entry' ? styles.detailEntry : ''}`}
    data-nc-report-reference={reference ? '' : undefined} onToggle={(event) => onToggle?.(event.currentTarget.open)}>
    <summary className={styles.referenceSummary}>
      <Heading className={styles.referenceHead}>
        <span className={styles.referenceMarker}><Icon name="chevron-right" size="sm" /></span>
        <span className={styles.referenceTitle}>{title}</span>
        {meta !== undefined && <span className={styles.referenceCount}>{meta}</span>}
      </Heading>
    </summary>
    {children}
  </details>;
}
