import { describe, expect, it } from 'vitest';
import { calendarDate, calendarDayGroups, calendarRepeatLabel, calendarEntryIncludesDate, calendarInstant, calendarListOperation, calendarListedEntrySchema, calendarOccurrenceOn, calendarWriteOperation, shiftCalendarDate, type CalendarListedEntry } from './calendar.js';

describe('calendar time contracts', () => {
  it('uses the selected timezone across midnight and calendar boundaries', () => {
    expect(calendarDate(Date.parse('2026-10-01T20:00:00Z'), 'Asia/Shanghai')).toBe('2026-10-02');
    expect(shiftCalendarDate('2026-12-31', 1)).toBe('2027-01-01');
    expect(shiftCalendarDate('2028-03-01', -1)).toBe('2028-02-29');
    expect(calendarInstant('2026-10-02T14:00', 'Asia/Shanghai')).toBe('2026-10-02T14:00:00+08:00');
  });
  it('rejects nonexistent dates and ambiguous or missing DST wall times', () => {
    expect(() => calendarInstant('2026-02-30T14:00', 'Asia/Shanghai')).toThrow();
    expect(() => calendarInstant('2026-03-08T02:30', 'America/New_York')).toThrow(/missing or ambiguous/);
    expect(() => calendarInstant('2026-11-01T01:30', 'America/New_York')).toThrow(/missing or ambiguous/);
    expect(calendarInstant('2026-11-01T01:30:00-04:00', 'America/New_York')).toBe('2026-11-01T01:30:00-04:00');
  });
  it('preserves retries and revisions in the real request contract', () => {
    const task = { title: 'Research', description: '', schedule: { kind: 'all_day' as const, date: '2026-10-02' } };
    const create = { task, idempotency_key: 'receipt' };
    expect(calendarWriteOperation(create).body).toEqual(create);
    expect(calendarWriteOperation({ id: 'a/b', expected_version: 2, task, cancelled: true })).toMatchObject({ path: '/api/calendar/tasks/a%2Fb', body: { expected_version: 2, cancelled: true } });
    expect(calendarListOperation('2026-10-02', '2026-10-03', 'Asia/Shanghai').path).toContain('Asia%2FShanghai');
  });
});

const listed = (schedule: CalendarListedEntry['task']['schedule'], occurrences: CalendarListedEntry['occurrences']): CalendarListedEntry => ({
  id: 'entry', task: { title: 'Review', description: '', schedule }, version: 1, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1, occurrences,
});

it('projects exclusive ends with server precision in the display timezone', () => {
  const span = { start: '2026-10-02T23:00:00+08:00', end: '2026-10-03T00:00:00.000001+08:00' };
  const entry = listed({ kind: 'timed', ...span, timezone: 'Asia/Shanghai' }, [span]);
  expect(calendarEntryIncludesDate(entry, '2026-10-03', 'Asia/Shanghai')).toBe(true);
  expect(calendarEntryIncludesDate(entry, '2026-10-03', 'UTC')).toBe(false);
  const exact = { ...span, end: '2026-10-03T00:00:00+08:00' };
  expect(calendarEntryIncludesDate(listed({ kind: 'timed', ...exact, timezone: 'Asia/Shanghai' }, [exact]), '2026-10-03', 'Asia/Shanghai')).toBe(false);
  expect(calendarEntryIncludesDate(listed({ kind: 'all_day', date: '2026-10-03' }, []), '2026-10-03', 'UTC')).toBe(true);
});

it('decodes a listed weekly entry and places each projected occurrence on its display date', () => {
  const wire = { ...listed({ kind: 'weekly', weekdays: ['mon', 'wed'], start: '09:30', end: '10:00', timezone: 'Asia/Shanghai', from: '2026-10-05' }, [
    { start: '2026-10-05T09:30:00+08:00', end: '2026-10-05T10:00:00+08:00' },
    { start: '2026-10-07T09:30:00+08:00', end: '2026-10-07T10:00:00+08:00' },
  ]) };
  const entry = calendarListedEntrySchema.parse(wire);
  expect(entry.task.schedule).toEqual(wire.task.schedule);
  expect(['2026-10-04', '2026-10-05', '2026-10-06', '2026-10-07'].map((day) => calendarEntryIncludesDate(entry, day, 'Asia/Shanghai'))).toEqual([false, true, false, true]);
  // Monday morning in Shanghai is Sunday evening in Los Angeles.
  expect(calendarOccurrenceOn(entry, '2026-10-04', 'America/Los_Angeles')?.start).toBe('2026-10-05T09:30:00+08:00');
  expect(calendarListedEntrySchema.safeParse({ ...wire, task: { ...wire.task, schedule: { ...wire.task.schedule, weekdays: ['monday'] } } }).success).toBe(false);
  expect(calendarListedEntrySchema.safeParse({ ...wire, occurrences: undefined }).success).toBe(false);
});

function repeatEntry(hour: string, overrides: Partial<CalendarListedEntry> = {}): CalendarListedEntry {
  const span = { start: `2026-10-22T${hour}:00+08:00`, end: `2026-10-22T${hour}:30+08:00` };
  return { ...listed({ kind: 'weekly', weekdays: ['thu'], start: hour, end: '23:59', timezone: 'Asia/Shanghai', from: '2026-10-01' }, [span]),
    id: hour, source_track_id: 'track', ...overrides,
    task: overrides.task ?? { title: `Check ${hour}（messages）`, description: 'Read messages.', schedule: { kind: 'weekly', weekdays: ['thu'], start: hour, end: '23:59', timezone: 'Asia/Shanghai', from: '2026-10-01' } } };
}

it('groups only matching weekly commitments on the selected display day and preserves every original ID', () => {
  const entries = ['09:00', '09:30', '10:00'].map((hour) => repeatEntry(hour));
  const groups = calendarDayGroups(entries.toReversed(), '2026-10-22', 'Asia/Shanghai');
  expect(groups).toHaveLength(1);
  expect(groups[0]).toMatchObject({ title: 'Check（messages）', intervalMinutes: 30 });
  expect(groups[0].entries.map((entry) => entry.id)).toEqual(['09:00', '09:30', '10:00']);
  expect(entries[0].task.title).toBe('Check 09:00（messages）');
  expect(calendarDayGroups(entries, '2026-10-23', 'Asia/Shanghai')).toEqual([]);
  expect(calendarDayGroups(entries, '2026-10-21', 'America/Los_Angeles')).toHaveLength(1);
  const irregular = calendarDayGroups([entries[0], entries[1], repeatEntry('11:00')], '2026-10-22', 'Asia/Shanghai');
  expect(irregular[0].intervalMinutes).toBeNull();
});

it('keeps different sources, instructions, titles, recurrence bounds, and coincident entries separate', () => {
  const first = repeatEntry('09:00');
  const next = repeatEntry('09:30');
  const separate = (entry: CalendarListedEntry) => expect(calendarDayGroups([first, entry], '2026-10-22', 'Asia/Shanghai')).toHaveLength(2);
  separate({ ...next, source_track_id: 'other' });
  separate({ ...next, created_by: 'other' });
  separate({ ...next, task: { ...next.task, description: 'Different instructions.' } });
  separate({ ...next, task: { ...next.task, title: 'Other check 09:30（messages）' } });
  separate({ ...next, task: { ...next.task, schedule: { ...next.task.schedule, kind: 'weekly', weekdays: ['thu'], start: '09:30', end: '23:59', timezone: 'Asia/Shanghai', from: '2026-10-02' } } });
  separate({ ...first, id: 'duplicate' });
  expect(calendarDayGroups([first, next].map((entry) => ({ ...entry, source_track_id: null })), '2026-10-22', 'Asia/Shanghai')).toHaveLength(2);
  const timed = [first, next].map((entry) => ({ ...entry, task: { ...entry.task, schedule: { kind: 'timed' as const, ...entry.occurrences[0], timezone: 'Asia/Shanghai' } } }));
  expect(calendarDayGroups(timed, '2026-10-22', 'Asia/Shanghai')).toHaveLength(2);
});


it('uses short recurrence wording for minutes, hours, daily and weekday schedules', () => {
  const group = (entries: CalendarListedEntry[], intervalMinutes: number | null = null) => ({ title: 'Check', entries, intervalMinutes });
  expect(calendarRepeatLabel(group([repeatEntry('09:00'), repeatEntry('09:30')], 30))).toBe('每 30 分钟');
  expect(calendarRepeatLabel(group([repeatEntry('09:00'), repeatEntry('11:00')], 120))).toBe('每 2 小时');
  expect(calendarRepeatLabel(group([repeatEntry('09:00'), repeatEntry('10:30')], 90))).toBe('每 90 分钟');
  const entry = repeatEntry('09:00');
  expect(calendarRepeatLabel(group([entry]))).toBe('周四 09:00');
  const weekly = (weekdays: ('mon' | 'tue' | 'wed' | 'thu' | 'fri' | 'sat' | 'sun')[]) => ({ ...entry, task: { ...entry.task, schedule: { kind: 'weekly' as const, weekdays, start: '09:00', end: '10:00', timezone: 'Asia/Shanghai', from: '2026-10-01' } } });
  expect(calendarRepeatLabel(group([weekly(['sun', 'sat', 'fri', 'thu', 'wed', 'tue', 'mon'])]))).toBe('每天 09:00');
  expect(calendarRepeatLabel(group([weekly(['mon', 'tue', 'wed', 'thu', 'fri'])]))).toBe('工作日 09:00');
  expect(calendarRepeatLabel(group([weekly(['fri', 'mon', 'wed'])]))).toBe('周一、三、五 09:00');
  expect(calendarRepeatLabel(group([entry, repeatEntry('11:00'), repeatEntry('14:00')]))).toBe('周四 3 次');
  expect(calendarRepeatLabel(group([listed({ kind: 'all_day', date: '2026-10-22' }, [])]))).toBeNull();
});
