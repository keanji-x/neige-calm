import { Button } from '@astryxdesign/core/Button';
import { DateInput } from '@astryxdesign/core/DateInput';
import { TimeInput } from '@astryxdesign/core/TimeInput';
import { TextInput } from '@astryxdesign/core/TextInput';
import { TextArea } from '@astryxdesign/core/TextArea';
import { Banner } from '@astryxdesign/core/Banner';
import type { ISODateString } from '@astryxdesign/core/Calendar';
import type { ISOTimeString } from '@astryxdesign/core/utils';
import { useState } from '../../ui/state/public.ts';
import { calendarInstant, shiftCalendarDate, wallTime, type CalendarEntry, type CalendarWrite } from '../../../../core/domain/calendar.ts';
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
  const [error, setError] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<{ fingerprint: string; key: string } | null>(null);
  const [selectedDate, setSelectedDate] = useState(date);
  if (!entry && selectedDate !== date) {
    const delta = (Date.parse(`${date}T12:00:00Z`) - Date.parse(`${selectedDate}T12:00:00Z`)) / 86400000;
    setSelectedDate(date); setDay(date); setEndDay(shiftCalendarDate(endDay, delta));
  }
  const changeDay = (value: string | undefined) => {
    if (value) { if (endDay === day) setEndDay(value); setDay(value); }
  };
  const save = async (cancelled: boolean) => {
    try {
      if (!title.trim()) throw new Error('Give this task a name.');
      const resolveTime = (value: string, edge: 'start' | 'end') => original?.kind === 'timed'
        && zone === original.timezone && wallTime(Date.parse(original[edge]), zone) === value
        ? original[edge] : calendarInstant(value, zone);
      const task = { title, description, schedule: !timed ? { kind: 'all_day' as const, date: day } : { kind: 'timed' as const, start: resolveTime(`${day}T${start}`, 'start'), end: resolveTime(`${endDay}T${end}`, 'end'), timezone: zone } };
      const fingerprint = JSON.stringify(task);
      const key = receipt?.fingerprint === fingerprint ? receipt.key : crypto.randomUUID();
      setReceipt({ fingerprint, key }); setError(null);
      await onSave(entry ? { id: entry.id, expected_version: entry.version, task, cancelled } : { idempotency_key: key, task });
      onClose();
    } catch (reason) { setError(reason instanceof Error ? reason.message : 'Could not save this task.'); }
  };
  return <form className={styles.form} onSubmit={(event) => { event.preventDefault(); void save(false); }}>
    <div className={styles.quickAdd}>
      <TextInput label="Task name" isLabelHidden={!entry} placeholder="What would you like to get done?" value={title} onChange={setTitle} isDisabled={pending} width="100%" />
      {!entry && <Button label="Add task" type="submit" variant="primary" isDisabled={pending || !title.trim()} isLoading={pending} />}
    </div>
    {entry && <DateInput label="Date" value={day as ISODateString} onChange={changeDay} isDisabled={pending} />}
    <div className={styles.options}>
      <Button label={timed ? 'Remove time' : 'Set time'} size="sm" variant="ghost" isDisabled={pending} onClick={() => setTimed(!timed)} />
      <Button label={details ? 'Hide notes' : 'Add notes'} size="sm" variant="ghost" isDisabled={pending} onClick={() => setDetails(!details)} />
    </div>
    {timed && zone !== timezone && <p className={styles.time}>Times in {zone}</p>}
    {timed && <div className={styles.timeFields}>
      <TimeInput label="Start" hourFormat="24h" value={start as ISOTimeString || undefined} onChange={(value) => setStart(value ?? '')} isDisabled={pending} width="100%" />
      <TimeInput label="End" hourFormat="24h" value={end as ISOTimeString || undefined} onChange={(value) => setEnd(value ?? '')} isDisabled={pending} width="100%" />
      <Button label={advanced ? 'Fewer options' : 'More time options'} size="sm" variant="ghost" onClick={() => setAdvanced(!advanced)} isDisabled={pending} />
      {(advanced || endDay !== day) && <DateInput label="End date" value={endDay as ISODateString} onChange={(value) => { if (value) setEndDay(value); }} isDisabled={pending} width="100%" />}
      {advanced && <TextInput label="Time zone" value={zone} onChange={setZone} isDisabled={pending} width="100%" />}
    </div>}
    {details && <TextArea label="Notes" value={description} onChange={setDescription} isDisabled={pending} />}
    {error && <Banner status="error" title={error} />}
    {entry && <div className={styles.actions}>
      <Button label="Save changes" type="submit" variant="primary" isDisabled={pending} isLoading={pending} />
      <Button label="Cancel task" variant="destructive" isDisabled={pending} onClick={() => { void save(true); }} />
      <Button label="Close" variant="ghost" isDisabled={pending} onClick={onClose} />
    </div>}
  </form>;
}
