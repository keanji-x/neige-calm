import { Button } from '@astryxdesign/core/Button';
import { DateInput } from '@astryxdesign/core/DateInput';
import { TimeInput } from '@astryxdesign/core/TimeInput';
import { TextInput } from '@astryxdesign/core/TextInput';
import { TextArea } from '@astryxdesign/core/TextArea';
import { Switch } from '@astryxdesign/core/Switch';
import { Banner } from '@astryxdesign/core/Banner';
import type { ISODateString } from '@astryxdesign/core/Calendar';
import type { ISOTimeString } from '@astryxdesign/core/utils';
import { useOperationFeedback } from '../../ui/operation-feedback/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  calendarInstant, calendarWriteFailureText, wallTime, type CalendarDraft, type CalendarEntry, type CalendarWrite,
} from '../../../../core/domain/calendar.ts';
import styles from './calendar.module.css';

export function CalendarEditor({ entry, date, timezone, pending, onClose, onSave }: Readonly<{
  entry: CalendarEntry | null; date: string; timezone: string; pending: boolean; onClose(): void; onSave(write: CalendarWrite): Promise<void>;
}>) {
  const original = entry?.task.schedule;
  const initialStart = original?.kind === 'timed' ? wallTime(Date.parse(original.start), original.timezone) : '';
  const initialEnd = original?.kind === 'timed' ? wallTime(Date.parse(original.end), original.timezone) : '';
  const [title, setTitle] = useState(entry?.task.title ?? '');
  const [description, setDescription] = useState(entry?.task.description ?? '');
  const [timed, setTimed] = useState(original?.kind === 'timed');
  const [details, setDetails] = useState(Boolean(entry?.task.description));
  const [advanced, setAdvanced] = useState(false);
  const [day, setDay] = useState(original?.kind === 'all_day' ? original.date : initialStart.slice(0, 10) || date);
  const [endDay, setEndDay] = useState(initialEnd.slice(0, 10) || date);
  const [start, setStart] = useState(initialStart.slice(11));
  const [end, setEnd] = useState(initialEnd.slice(11));
  const [zone, setZone] = useState(original?.kind === 'timed' ? original.timezone : timezone);
  /* The draft's own checks, said before anything is sent; a sent write's failure is the feedback's. */
  const [error, setError] = useState<string | null>(null);
  const feedback = useOperationFeedback();
  const [receipt, setReceipt] = useState<{ fingerprint: string; key: string } | null>(null);
  const changeDay = (value: string | undefined) => {
    if (value) { if (endDay === day) setEndDay(value); setDay(value); }
  };
  const draftWrite = (cancelled: boolean): CalendarWrite => {
    if (cancelled && entry) return { id: entry.id, expected_version: entry.version, task: entry.task, cancelled: true };
    if (timed && (!start || !end)) throw new Error('Choose a start and end time.');
    if (!title.trim()) throw new Error('Give this task a name.');
    const resolveTime = (value: string, edge: 'start' | 'end') => original?.kind === 'timed'
      && zone === original.timezone && wallTime(Date.parse(original[edge]), zone) === value
      ? original[edge] : calendarInstant(value, zone);
    const task = { title, description, schedule: !timed ? { kind: 'all_day' as const, date: day } : { kind: 'timed' as const, start: resolveTime(`${day}T${start}`, 'start'), end: resolveTime(`${endDay}T${end}`, 'end'), timezone: zone } };
    const fingerprint = JSON.stringify(task);
    const key = receipt?.fingerprint === fingerprint ? receipt.key : crypto.randomUUID();
    setReceipt({ fingerprint, key });
    return entry ? { id: entry.id, expected_version: entry.version, task, cancelled } : { idempotency_key: key, task };
  };
  const save = async (cancelled: boolean) => {
    let write: CalendarWrite;
    try { write = draftWrite(cancelled); } catch (reason) {
      feedback.clear(); setError(reason instanceof Error ? reason.message : 'Could not save this task.'); return;
    }
    setError(null);
    if (await feedback.run(onSave(write), calendarWriteFailureText(write))) onClose();
  };
  const shown = error ?? feedback.error;
  return <form className={styles.form} onSubmit={(event) => { event.preventDefault(); void save(false); }}>
    <TextInput label="Task title" isLabelHidden placeholder="Task title" hasAutoFocus size="lg" value={title} onChange={setTitle} isDisabled={pending} width="100%" />
    <div className={styles.dateRow}>
      <DateInput label="Date" value={day as ISODateString} onChange={changeDay} isDisabled={pending} width="100%" />
      <Switch label="All day" value={!timed} onChange={(allDay) => setTimed(!allDay)} isDisabled={pending} />
    </div>
    {timed && zone !== timezone && <p className={styles.time}>Times in {zone}</p>}
    {timed && <div className={styles.timeFields}>
      <TimeInput label="Start" hourFormat="24h" value={start as ISOTimeString || undefined} onChange={(value) => setStart(value ?? '')} isDisabled={pending} width="100%" />
      <TimeInput label="End" hourFormat="24h" value={end as ISOTimeString || undefined} onChange={(value) => setEnd(value ?? '')} isDisabled={pending} width="100%" />
      <Button label={advanced ? 'Fewer options' : 'More time options'} size="sm" variant="ghost" onClick={() => setAdvanced(!advanced)} isDisabled={pending} />
      {(advanced || endDay !== day) && <DateInput label="End date" value={endDay as ISODateString} onChange={(value) => { if (value) setEndDay(value); }} isDisabled={pending} width="100%" />}
      {advanced && <TextInput label="Time zone" value={zone} onChange={setZone} isDisabled={pending} width="100%" />}
    </div>}
    {details ? <TextArea label="Notes" placeholder="Add context or an expected result" value={description} onChange={setDescription} isDisabled={pending} />
      : <div><Button label="Add notes" size="sm" variant="ghost" isDisabled={pending} onClick={() => setDetails(true)} /></div>}
    {shown !== null && <Banner status="error" title={shown} />}
    <div className={styles.actions}>
      {entry && <Button label="Cancel task" variant="destructive" isDisabled={pending} onClick={() => { void save(true); }} />}
      <div className={styles.saveActions}>
        <Button label="Cancel" variant="ghost" isDisabled={pending} onClick={onClose} />
        <Button label={entry ? 'Save changes' : 'Create task'} type="submit" variant="primary" isDisabled={pending || !title.trim()} isLoading={pending} />
      </div>
    </div>
  </form>;
}

/** A weekly entry has no recurrence editor here: it is shown read-only and changed through its Track. */
export function WeeklyEntryDetails({ task, schedule }: Readonly<{
  task: CalendarDraft; schedule: Extract<CalendarDraft['schedule'], { kind: 'weekly' }>;
}>) {
  const days = schedule.weekdays.map((day) => day[0].toUpperCase() + day.slice(1)).join(', ');
  return <div className={styles.form}>
    <p>{task.title}</p>
    <p className={styles.time}>{days} · {schedule.start} – {schedule.end} · {schedule.timezone}<br />
      From {schedule.from}{schedule.until === undefined ? '' : ` through ${schedule.until}`}</p>
    {task.description && <p>{task.description}</p>}
    <Banner status="info" title="Repeats weekly — edit it through the Track." />
  </div>;
}
