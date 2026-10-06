// Shared report appendix disclosure, also used by the report's Reference section.
import type { ReactNode } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { SpringRotation } from '../../../ui/motion/rotation.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './document.module.css';

export function ReportDetails({ title, meta, layout, variant = 'section', reference = false, onToggle, children }: Readonly<{
  title: string; meta?: ReactNode; layout: 'grid' | 'appendix'; variant?: 'section' | 'entry';
  reference?: boolean; onToggle?: (open: boolean) => void; children: ReactNode;
}>) {
  const [expanded, setExpanded] = useState(false);
  const Heading = variant === 'section' ? 'h2' : 'h3';
  return <details className={`${styles.reference} ${layout === 'appendix' ? styles.appendix : ''} ${variant === 'entry' ? styles.detailEntry : ''}`}
    data-nc-report-reference={reference ? '' : undefined} onToggle={event => { setExpanded(event.currentTarget.open); onToggle?.(event.currentTarget.open); }}>
    <summary className={styles.referenceSummary}>
      <Heading className={styles.referenceHead}>
        <SpringRotation className={styles.referenceMarker} angle={expanded ? 90 : 0}><Icon name="chevron-right" size="sm" /></SpringRotation>
        <span className={styles.referenceTitle}>{title}</span>
        {meta !== undefined && <span className={styles.referenceCount}>{meta}</span>}
      </Heading>
    </summary>
    {children}
  </details>;
}
