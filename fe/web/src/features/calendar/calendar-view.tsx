import { useEffect, useMemo, useRef, type ReactNode } from 'react';
import { useState } from '../../ui/state/public.ts';
import { SegmentedControl, SegmentedControlItem } from '@astryxdesign/core/SegmentedControl';
import { Icon } from '../../ui/icon/public.tsx';
import { PanelModule } from '../../ui/panel-card/public.tsx';
import FullCalendar, { type CalendarRef } from '@fullcalendar/react';
import dayGridPlugin from '@fullcalendar/react/daygrid';
import interactionPlugin from '@fullcalendar/react/interaction';
import themePlugin from '@fullcalendar/react/themes/monarch';
import { calendarDate, calendarScheduleIncludesDate, type CalendarEntry, type CalendarWindow } from '../../../../core/domain/calendar.ts';
import styles from './calendar.module.css';

export function TaskCalendar({ date, timezone, entries, children, trackCountOn, onDateChange, onWindowChange }: Readonly<{
  trackCountOn?: (date: string) => number | null;
  children: ReactNode;
  date: string; timezone: string; entries: readonly CalendarEntry[];
  onDateChange(date: string): void; onWindowChange(window: CalendarWindow): void;
}>) {
  const [mode, setMode] = useState('week');
  const plugins = useMemo(() => [dayGridPlugin, interactionPlugin, themePlugin], []);
  const calendar = useRef<CalendarRef>(null);
  useEffect(() => {
    const api = calendar.current?.getApi();
    if (api && calendarDate(api.getDate().getTime(), timezone) !== date) api.gotoDate(date);
  }, [date, timezone]);
  const choose = (day: string) => onDateChange(day);
  const countsOn = (value: Date) => {
    const day = calendarDate(value.getTime(), timezone);
    const tracks = trackCountOn?.(day);
    const tasks = entries.filter((entry) => calendarScheduleIncludesDate(entry.task.schedule, day, timezone)).length;
    return { day, tracks, tasks };
  };
  const dateBadge = (value: Date) => {
    const { day, tracks, tasks } = countsOn(value);
    return <span className={[styles.dateBadge, day === date ? styles.selectedDate : ''].filter(Boolean).join(' ')}>
      <span className={styles.dayNumber}>{new Intl.DateTimeFormat('en-US', { day: 'numeric', timeZone: timezone }).format(value)}</span>
      <span className={styles.dateCounts}>
        {tracks != null && tracks > 0 && <sup title={`${tracks} tracks`} aria-label={`${tracks} tracks`}>{tracks}</sup>}
        {tasks > 0 && <sub title={`${tasks} tasks`} aria-label={`${tasks} tasks`}>{tasks}</sub>}
      </span>
    </span>;
  };
  return <PanelModule title="Calendar" grow action={<SegmentedControl label="Calendar view" size="sm" value={mode}
    onChange={(value) => { setMode(value); calendar.current?.getApi().changeView(value === 'week' ? 'dayGridWeek' : 'dayGridMonth'); }}>
    <SegmentedControlItem value="week" label="Week" />
    <SegmentedControlItem value="month" label="Month" />
  </SegmentedControl>}><section className={styles.calendar} aria-label="Calendar tasks">
    <FullCalendar ref={calendar} plugins={plugins} initialView="dayGridWeek" initialDate={date}
    className={styles.monthGrid} viewClass={styles.monthView} borderless headerToolbarClass={styles.monthToolbar} headerToolbar={{ start: 'prev', center: 'title', end: 'next' }}
    buttons={{ prev: { iconContent: () => <Icon name="chevron-left" size="sm" /> }, next: { iconContent: () => <Icon name="chevron-right" size="sm" /> } }}
    buttonClass={styles.navigationButton} toolbarTitleClass={styles.monthTitle} titleFormat={{ year: 'numeric', month: 'long' }} firstDay={1} fixedWeekCount={false} height="auto"
    dayHeaderFormat={mode === 'week' ? { weekday: 'short', day: 'numeric' } : { weekday: 'short' }}
    dayHeaderContent={(info) => <span className={styles.dayHeader}>
      <span>{new Intl.DateTimeFormat('en-US', { weekday: 'short', timeZone: timezone }).format(info.date)}</span>
      {mode === 'week' && dateBadge(info.date)}
    </span>}
    timeZone={timezone} navLinks navLinkClass={styles.dateLink}
    dayRowClass={mode === 'week' ? styles.emptyWeekRow : undefined}
    navLinkHint={(label, value) => { const { day, tracks, tasks } = countsOn(value); return `Go to ${label}${day === date ? ', selected' : ''}${tracks == null ? '' : `, ${tracks} tracks`}, ${tasks} tasks`; }}
    dayCellClass={mode === 'week' ? styles.weekCell : styles.countCell} dayCellTopClass={styles.dayTop}
    dayCellTopContent={(info) => dateBadge(info.date)}
    datesSet={(info) => {
      onWindowChange({ from: calendarDate(info.start.getTime(), timezone), until: calendarDate(info.end.getTime(), timezone) });
      onDateChange(calendarDate(info.view.calendar.getDate().getTime(), timezone));
    }}
    dateClick={(info) => choose(calendarDate(info.date.getTime(), timezone))}
    navLinkDayClick={(day) => choose(calendarDate(day.getTime(), timezone))} />{children}</section></PanelModule>;
}
