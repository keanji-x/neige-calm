// Optional report evidence follows the ordinary document, without additional page chrome.
import type { ReactNode } from 'react';
import styles from './daily.module.css';

export function DailyReportEvidence({ date, onToggle, children }: Readonly<{
  date: string; onToggle: (open: boolean) => void; children: ReactNode;
}>) {
  return <details className={styles.evidence} onToggle={(event) => onToggle(event.currentTarget.open)}>
    <summary>Report changes · {date}</summary>
    {children}
  </details>;
}
