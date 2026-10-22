import { useEffect, useRef } from 'react';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { PanelModule } from '../../ui/panel-card/public.tsx';
import { EmptyState } from '../../ui/empty-state/public.tsx';
import { Button } from '@astryxdesign/core/Button';
import { Banner } from '@astryxdesign/core/Banner';
import { List, ListItem } from '@astryxdesign/core/List';
import { useState } from '../../ui/state/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { ListText } from '../../ui/list-typography/public.tsx';
import { Dialog, type DialogOrigin } from '../../ui/dialog/public.tsx';
import { calendarDate, calendarDayGroups, calendarOccurrenceOn, calendarRepeatLabel, type CalendarDayGroup, type CalendarListedEntry, type CalendarEdit, type CalendarWindow } from '../../../../core/domain/calendar.ts';
import { CalendarEditor, WeeklyEntryDetails } from './editor.tsx';
import type { CalendarGridHandle } from './calendar-view.tsx';
import { loadCalendarGrid } from './grid-loader.ts';
import styles from './calendar.module.css';

type GridState = { kind: 'loading' } | { kind: 'failed' } | { kind: 'ready'; Grid: typeof import('./calendar-view.tsx').TaskCalendar };

export type CalendarEntriesView = Readonly<{ entries: readonly CalendarListedEntry[] | undefined; loading: boolean; error: string | null }>;
export type CalendarTasksProps = Readonly<{
  trackCountOn?: (date: string) => number | null;
  date: string; timezone: string; month: CalendarEntriesView; day: CalendarEntriesView;
  enabled: boolean; pending: boolean;
  onDateChange(date: string): void; onWindowChange(window: CalendarWindow): void;
  onRetry(): void; onReload(): void; onSettings(): void;
  onOpenTrack(id: string): void; onSave(write: CalendarEdit): Promise<void>;
}>;
export function CalendarTasks({ date, timezone, trackCountOn, month, day, enabled, pending, onDateChange, onWindowChange, onRetry, onReload, onSettings, onOpenTrack, onSave }: CalendarTasksProps) {
  const [mode, setMode] = useState('week');
  const gridControls = useRef<CalendarGridHandle>(null);
  const [grid, setGrid] = useState<GridState>({ kind: 'loading' });
  useEffect(() => {
    let cancelled = false;
    setGrid({ kind: 'loading' });
    void loadCalendarGrid().then(
      module => { if (!cancelled) setGrid({ kind: 'ready', Grid: module.TaskCalendar }); },
      () => { if (!cancelled) setGrid({ kind: 'failed' }); },
    );
    return () => { cancelled = true; };
  }, [setGrid]);
  const createRef = useRef<HTMLSpanElement | null>(null);
  const [editing, setEditing] = useState<Readonly<{ entry: CalendarListedEntry | null; date: string; origin: DialogOrigin }> | null>(null);
  const [grouping, setGrouping] = useState<Readonly<{ group: CalendarDayGroup; date: string; origin: DialogOrigin }> | null>(null);
  const groups = calendarDayGroups(day.entries ?? [], date, timezone);
  const dateLabel = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric', weekday: 'short', timeZone: 'UTC' }).format(new Date(`${date}T12:00:00Z`));
  const open = (entry: CalendarListedEntry, origin: DialogOrigin) => setEditing({ entry, date, origin });
  const schedule = editing?.entry?.task.schedule;
  const weekly = schedule?.kind === 'weekly' ? schedule : null;
  return <>
    <PanelModule title="Calendar" icon="calendar" className={styles.calendarModule} grow action={<span className={styles.viewControl}><SegmentedControl label="Calendar view" size="sm" value={mode}
      onChange={value => { setMode(value); gridControls.current?.changeMode(value); }}>
      <SegmentedControlItem value="week" label="Week" />
      <SegmentedControlItem value="month" label="Month" />
    </SegmentedControl></span>}><section className={styles.calendar} aria-label="Calendar tasks">
    {grid.kind === 'ready' && <grid.Grid trackCountOn={trackCountOn} date={date} timezone={timezone} mode={mode} controls={gridControls} entries={enabled ? month.entries ?? [] : []}
      onDateChange={onDateChange} onWindowChange={onWindowChange} />}
    {grid.kind === 'failed' && <Banner status="error" title="The calendar grid could not load."
      description="You can still use this day's tasks. Save your work first. Reloading clears unsaved changes."
      endContent={<Button label="Reload page" variant="ghost" isDisabled={editing !== null || grouping !== null || pending} onClick={onReload} />} />}
    {month.error && <Banner status="error" title={month.error} endContent={<Button label="Retry" variant="ghost" onClick={onRetry} />} />}
    {(grid.kind === 'loading' || (grid.kind === 'ready' && month.loading)) && <p role="status" className={styles.empty}>Loading calendar…</p>}
    {!month.loading && !month.error && !enabled && <p className={styles.empty}>Calendar is temporarily unavailable.<br /><Button label="Open plugin settings" variant="ghost" onClick={onSettings} /></p>}
    <section className={styles.dayDetails} aria-label="Selected day tasks">
      <div className={styles.heading}>
        <div className={styles.dateHeading}><ListText as="h2" tone="group">{dateLabel}</ListText><ListText tone="secondary">{timezone}</ListText></div>
        <span ref={createRef} style={{ display: 'inline-flex' }}><Button label="New task" isIconOnly icon={<Icon name="plus" size="sm" />} className={styles.iconAction} size="sm" variant="ghost" isDisabled={!enabled || pending} onClick={() => setEditing({ entry: null, date, origin: { kind: 'anchor', anchor: { ref: createRef }, unavailable: 'surface-center' } })} /></span>
      </div>
      <div className={styles.taskScroll} role="region" aria-label="Task list">
      {enabled && (day.error ? <Banner status="error" title={day.error} endContent={<Button label="Retry" variant="ghost" onClick={onRetry} />} />
        : day.loading ? <p role="status" className={styles.empty}>Loading tasks…</p>
        : day.entries?.length === 0 ? <EmptyState variant="compact" icon={<Icon name="tasks" />} title="这一天没有安排任务" description="这一天的日程任务会显示在这里。" />
        : <List density="compact">
            {groups.map((group) => {
              const entry = group.entries[0];
              const repeat = calendarRepeatLabel(group);
              const zone = group.entries.length === 1 && entry.task.schedule.kind === 'weekly' && entry.task.schedule.timezone !== timezone ? ` · ${entry.task.schedule.timezone}` : '';
              return <CalendarEntryRow key={entry.id} entry={entry} date={date} timezone={timezone} title={group.title}
                label={repeat === null ? scheduleLabel(entry, date, timezone) : `↻ ${repeat}${zone}`}
                onOpen={(_, origin) => group.entries.length > 1 ? setGrouping({ group, date, origin }) : open(entry, origin)} />;
            })}
          </List>)}
      </div>
    </section>
    </section></PanelModule>
    {grouping !== null && <Dialog origin={grouping.origin} open onClose={() => setGrouping(null)} title={grouping.group.title}>
      <p className={styles.time}>{grouping.date} · {timezone} · {groupLabel(grouping.group, grouping.date, timezone)}</p>
      <List density="compact">
        {grouping.group.entries.map((entry) => <CalendarEntryRow key={entry.id} entry={entry} date={grouping.date} timezone={timezone}
          onOpen={(_, origin) => { setEditing({ entry, date: grouping.date, origin }); setGrouping(null); }} />)}
      </List>
    </Dialog>}
    {editing !== null && <Dialog origin={editing.origin} open onClose={() => { if (!pending) setEditing(null); }} title={weekly ? 'Weekly task' : editing.entry ? 'Edit task' : 'New task'}>
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

function groupLabel(group: CalendarDayGroup, date: string, timezone: string): string {
  const time = (entry: CalendarListedEntry) => {
    const start = calendarOccurrenceOn(entry, date, timezone)!.start;
    return new Intl.DateTimeFormat('en-US', { timeZone: timezone, hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }).format(new Date(start));
  };
  const interval = group.intervalMinutes === null ? '' : ` · Every ${group.intervalMinutes} min`;
  return `${time(group.entries[0])} – ${time(group.entries[group.entries.length - 1])}${interval} · ${group.entries.length} runs`;
}

function CalendarEntryRow({ entry, date, timezone, onOpen, title = entry.task.title, label = scheduleLabel(entry, date, timezone) }: {
  entry: CalendarListedEntry; date: string; timezone: string; title?: string; label?: string;
  onOpen: (entry: CalendarListedEntry, origin: DialogOrigin) => void;
}) {
  const ref = useRef<HTMLLIElement | null>(null);
  return <ListItem ref={ref} className={styles.taskItem} label={<span className={styles.detailRow}>
    <span aria-hidden="true" /><ListText tone="primary" className={styles.detailTitle}>{title}</ListText>
    <span className={styles.detailTime}>{label}</span>
  </span>} onClick={() => onOpen(entry, { kind: 'anchor', anchor: { ref }, unavailable: 'surface-center' })} />;
}
