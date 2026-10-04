// Date navigation owns no Track lifecycle. Track content and report changes are app-composed slots.
import type { ReactNode } from 'react';
import { Button } from '@astryxdesign/core/Button';
import { shiftDailyDate } from '../../../../core/domain/daily-planner.ts';
import styles from './daily.module.css';

export function DailyPage({ date, timeZone, onSelectDate, onOpenPlanner, changes, legacy, children }: Readonly<{
  date: string | null; timeZone: string | null; onSelectDate: (date?: string) => void;
  onOpenPlanner?: () => void;
  changes: ReactNode; legacy: ReactNode; children: ReactNode;
}>) {
  return <div className={styles.page}>
    <nav aria-label="Daily Planner dates" className={styles.dates}>
      <span className={styles.title}>Daily Planner</span>
      <Button label="Previous day" variant="ghost" size="sm" isDisabled={date === null} onClick={() => { if (date !== null) onSelectDate(shiftDailyDate(date, -1)); }}>Previous day</Button>
      <label className={styles.dateLabel}>Day <input type="date" aria-label="Planner date" value={date ?? ''}
        onChange={(event) => { if (event.currentTarget.value !== '') onSelectDate(event.currentTarget.value); }} /></label>
      <Button label="Next day" variant="ghost" size="sm" isDisabled={date === null} onClick={() => { if (date !== null) onSelectDate(shiftDailyDate(date, 1)); }}>Next day</Button>
      <Button label="Today" variant="secondary" size="sm" onClick={() => onSelectDate()}>Today</Button>
      {timeZone !== null && <span className={styles.zone}>{timeZone}</span>}
      <Button label="Open daily Planner" variant="primary" size="sm" isDisabled={onOpenPlanner === undefined} onClick={onOpenPlanner}>Open Planner</Button>
      {legacy}
    </nav>
    <div className={styles.changes}>{changes}</div>
    <div className={styles.track}>{children}</div>
  </div>;
}
