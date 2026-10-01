import { describe, expect, it } from 'vitest';
import { calendarDate, calendarInstant, calendarListOperation, calendarWriteOperation, shiftCalendarDate } from './calendar.js';

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
