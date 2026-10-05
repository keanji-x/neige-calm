import { Button } from '@astryxdesign/core/Button';
import { Banner } from '@astryxdesign/core/Banner';
import { List, ListItem } from '@astryxdesign/core/List';
import { useState } from '../../ui/state/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { ListText } from '../../ui/list-typography/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { calendarDate, calendarOccurrenceOn, type CalendarListedEntry, type CalendarEdit, type CalendarWindow } from '../../../../core/domain/calendar.ts';
import { CalendarEditor, WeeklyEntryDetails } from './editor.tsx';
import { TaskCalendar } from './calendar-view.tsx';
import styles from './calendar.module.css';

export type CalendarEntriesView = Readonly<{ entries: readonly CalendarListedEntry[] | undefined; loading: boolean; error: string | null }>;
export type CalendarTasksProps = Readonly<{
  trackCountOn?: (date: string) => number | null;
  date: string; timezone: string; month: CalendarEntriesView; day: CalendarEntriesView;
  enabled: boolean; pending: boolean;
  onDateChange(date: string): void; onWindowChange(window: CalendarWindow): void;
  onRetry(): void; onSettings(): void;
  onOpenTrack(id: string): void; onSave(write: CalendarEdit): Promise<void>;
}>;
export function CalendarTasks({ date, timezone, trackCountOn, month, day, enabled, pending, onDateChange, onWindowChange, onRetry, onSettings, onOpenTrack, onSave }: CalendarTasksProps) {
  const [editing, setEditing] = useState<Readonly<{ entry: CalendarListedEntry | null; date: string }> | null>(null);
  const dateLabel = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', weekday: 'short', timeZone: 'UTC' }).format(new Date(`${date}T12:00:00Z`));
  const open = (entry: CalendarListedEntry) => setEditing({ entry, date });
  const schedule = editing?.entry?.task.schedule;
  const weekly = schedule?.kind === 'weekly' ? schedule : null;
  return <>
    <TaskCalendar trackCountOn={trackCountOn} date={date} timezone={timezone} entries={enabled ? month.entries ?? [] : []}
      onDateChange={onDateChange} onWindowChange={onWindowChange}>
    {month.error && <Banner status="error" title={month.error} endContent={<Button label="Retry" variant="ghost" onClick={onRetry} />} />}
    {month.loading && <p role="status" className={styles.empty}>Loading calendar…</p>}
    {!month.loading && !month.error && !enabled && <p className={styles.empty}>Calendar is temporarily unavailable.<br /><Button label="Open plugin settings" variant="ghost" onClick={onSettings} /></p>}
    <section className={styles.dayDetails} aria-label="Selected day tasks">
      <div className={styles.heading}>
        <div className={styles.dateHeading}><h2>{dateLabel}</h2><span>{timezone}</span></div>
        <Button label="New task" isIconOnly icon={<Icon name="plus" size="sm" />} className={styles.iconAction} size="sm" variant="ghost" isDisabled={!enabled || pending} onClick={() => setEditing({ entry: null, date })} />
      </div>
      <div className={styles.taskScroll} role="region" aria-label="Task list">
      {enabled && (day.error ? <Banner status="error" title={day.error} endContent={<Button label="Retry" variant="ghost" onClick={onRetry} />} />
        : day.loading ? <p role="status" className={styles.empty}>Loading tasks…</p>
        : day.entries?.length === 0 ? <p className={styles.empty}>No tasks for this day.</p>
        : <List density="compact">
            {day.entries?.toSorted((a, b) => compareSchedule(a, b, date, timezone)).map((entry) => <ListItem key={entry.id} className={styles.taskItem} label={<span className={styles.detailRow}>
              <span aria-hidden="true" /><ListText tone="primary" className={styles.detailTitle}>{entry.task.title}</ListText>
              <span className={styles.detailTime}>{scheduleLabel(entry, date, timezone)}</span>
            </span>} onClick={() => open(entry)} />)}
          </List>)}
      </div>
    </section>
    </TaskCalendar>
    {editing !== null && <Dialog open onClose={() => { if (!pending) setEditing(null); }} title={weekly ? 'Weekly task' : editing.entry ? 'Edit task' : 'New task'}>
      {weekly && editing.entry ? <WeeklyEntryDetails task={editing.entry.task} schedule={weekly} />
        : <CalendarEditor key={editing.entry ? `${editing.entry.id}:${editing.entry.version}` : 'new'} entry={editing.entry}
          date={editing.date} timezone={timezone} pending={pending} onClose={() => setEditing(null)} onSave={onSave} />}
      {editing.entry?.source_track_id && <Button label="Source track" size="sm" variant="ghost" onClick={() => onOpenTrack(editing.entry!.source_track_id!)} />}
    </Dialog>}
  </>;
}
function scheduleLabel(entry: CalendarListedEntry, date: string, timezone: string): string {
  if (entry.task.schedule.kind === 'all_day') return 'All day';
  const span = calendarOccurrenceOn(entry, date, timezone);
  if (span === undefined) return '';
  const format = (value: string) => new Intl.DateTimeFormat('en-US', {
    timeZone: timezone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
    ...(calendarDate(Date.parse(value), timezone) !== date ? { month: 'short' as const, day: 'numeric' as const } : {}),
  }).format(new Date(value));
  return `${format(span.start)} – ${format(span.end)}${entry.task.schedule.kind === 'weekly' ? ' · Weekly' : ''}`;
}

/** All-day entries first, then by the start of the occurrence shown on `date`. */
function compareSchedule(a: CalendarListedEntry, b: CalendarListedEntry, date: string, timezone: string): number {
  const start = (entry: CalendarListedEntry) => calendarOccurrenceOn(entry, date, timezone)?.start;
  const left = start(a);
  const right = start(b);
  if (left === undefined) return right === undefined ? 0 : -1;
  if (right === undefined) return 1;
  return Date.parse(left) - Date.parse(right);
}
