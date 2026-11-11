// Shared report appendix disclosure, also used by the report's Reference section.
import { useCallback, useRef, type ReactNode } from 'react';
import { useState } from '../../../ui/state/public.ts';
import { SpringRotation } from '../../../ui/motion/rotation.tsx';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './document.module.css';

export function ReportDetails({ title, meta, layout, variant = 'section', reference = false, onToggle, children }: Readonly<{
  title: string; meta?: ReactNode; layout: 'grid' | 'appendix'; variant?: 'section' | 'entry' | 'card';
  reference?: boolean; onToggle?: (open: boolean) => void; children: ReactNode | ((close: () => void) => ReactNode);
}>) {
  const [expanded, setExpanded] = useState(false);
  const detailsRef = useRef<HTMLDetailsElement>(null);
  const summaryRef = useRef<HTMLElement>(null);
  const close = useCallback(() => {
    const details = detailsRef.current;
    const restoreFocus = details !== null && details.contains(document.activeElement);
    if (details !== null) details.open = false;
    setExpanded(false);
    onToggle?.(false);
    if (restoreFocus) summaryRef.current?.focus();
  }, [onToggle]);
  const Heading = variant === 'section' ? 'h2' : 'h3';
  return <details ref={detailsRef} className={`${styles.reference} ${layout === 'appendix' ? styles.appendix : ''} ${variant !== 'section' ? styles.detailEntry : ''} ${variant === 'card' ? styles.detailCard : ''}`}
    data-nc-report-reference={reference ? '' : undefined} onToggle={event => { setExpanded(event.currentTarget.open); onToggle?.(event.currentTarget.open); }}>
    <summary ref={summaryRef} className={styles.referenceSummary}>
      <Heading className={styles.referenceHead}>
        <SpringRotation className={styles.referenceMarker} angle={expanded ? 90 : 0}><Icon name="chevron-right" size="sm" /></SpringRotation>
        <span className={styles.referenceTitle}>{title}</span>
        {meta !== undefined && <span className={styles.referenceCount}>{meta}</span>}
      </Heading>
    </summary>
    {typeof children === 'function' ? children(close) : children}
  </details>;
}
