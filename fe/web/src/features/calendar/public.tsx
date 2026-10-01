import { Button } from '@astryxdesign/core/Button';
import { Banner } from '@astryxdesign/core/Banner';
import { useState } from '../../ui/state/public.ts';
import { Dialog } from '../../ui/dialog/public.tsx';
import { calendarDate, type CalendarEntry, type CalendarWrite } from '../../../../core/domain/calendar.ts';
import { CalendarEditor } from './editor.tsx';
import styles from './calendar.module.css';

export type CalendarTasksProps = Readonly<{
  date: string; timezone: string; entries: readonly CalendarEntry[] | undefined;
  enabled: boolean; loading: boolean; error: string | null; pending: boolean;
  onRetry(): void; onSettings(): void;
  onOpenTrack(id: string): void; onSave(write: CalendarWrite): Promise<void>;
}>;
export function CalendarTasks({ date, timezone, entries, enabled, loading, error, pending, onRetry, onSettings, onOpenTrack, onSave }: CalendarTasksProps) {
  const [editing, setEditing] = useState<CalendarEntry | null>(null);
  const [generation, setGeneration] = useState(0);
  return <section className={styles.calendar} aria-label="Calendar tasks">
    <div className={styles.agenda}>
      <div className={styles.heading}><h2>{new Intl.DateTimeFormat(undefined, { weekday: 'long', month: 'short', day: 'numeric', timeZone: 'UTC' }).format(new Date(`${date}T12:00:00Z`))}</h2><span>{timezone}</span></div>
      {error !== null ? <Banner status="error" title={error} endContent={<Button label="Retry" variant="ghost" onClick={onRetry} />} />
        : loading ? <p role="status">Loading calendar…</p>
        : !enabled ? <p>Arrange work here, or ask your assistant to add it.<br /><Button label="Enable Calendar in Settings" variant="ghost" onClick={onSettings} /></p>
        : null}
      {enabled && <CalendarEditor key={generation} entry={null} date={date} timezone={timezone} pending={pending} onClose={() => setGeneration((value) => value + 1)} onSave={onSave} />}
      {enabled && !error && entries && <ul className={styles.entries}>{[...entries].sort((a, b) => {
        const start = (entry: CalendarEntry) => entry.task.schedule.kind === 'all_day' ? -Infinity : Date.parse(entry.task.schedule.start);
        return start(a) - start(b) || a.task.title.localeCompare(b.task.title) || a.id.localeCompare(b.id);
      }).map((entry) => <li key={entry.id} className={styles.entry}>
        <div className={styles.entryHeading}>
          <Button label={entry.task.title} variant="ghost" onClick={() => setEditing(entry)} />
          <span className={styles.time}>{entry.task.schedule.kind === 'all_day' ? 'All day' : `${formatTime(entry.task.schedule.start, timezone, date)} – ${formatTime(entry.task.schedule.end, timezone, date)}`}</span>
        </div>
        {entry.task.description && <p>{entry.task.description}</p>}
        {entry.source_track_id && <Button label="Source track" variant="ghost" size="sm" onClick={() => onOpenTrack(entry.source_track_id!)} />}
      </li>)}</ul>}
      {enabled && !loading && !error && entries?.length === 0 && <p className={styles.empty}>Nothing planned yet.</p>}
    </div>
    {editing !== null && <Dialog open onClose={() => { if (!pending) setEditing(null); }} title="Edit calendar task">
      <CalendarEditor key={`${editing.id}:${editing.version}`} entry={editing} date={date} timezone={timezone} pending={pending} onClose={() => setEditing(null)} onSave={onSave} />
    </Dialog>}
  </section>;
}
function formatTime(value: string, timezone: string, date: string): string {
  const crossDay = calendarDate(Date.parse(value), timezone) !== date;
  return new Intl.DateTimeFormat(undefined, { timeZone: timezone, ...(crossDay ? { month: 'short' as const, day: 'numeric' as const } : {}), hour: '2-digit', minute: '2-digit' }).format(new Date(value));
}
