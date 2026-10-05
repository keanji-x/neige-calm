import { describe, expect, it } from 'vitest';
import { dailyTrackOperation, reportChangesOperation, reportEditsOperation, shiftDailyDate } from './daily-planner.js';
describe('daily Planner date and evidence contracts', () => {
  it('uses date-only arithmetic across month and year boundaries', () => {
    expect(shiftDailyDate('2026-01-01', -1)).toBe('2025-12-31');
    expect(shiftDailyDate('2028-03-01', -1)).toBe('2028-02-29');
  });
  it('keeps all reads side effect free and snapshot cursors explicit', () => {
    expect(dailyTrackOperation().method).toBe('GET');
    expect(dailyTrackOperation('2026-10-04').path).toBe('/api/today/daily?date=2026-10-04');
    expect(reportChangesOperation('2026-10-03', { cursor: 'a/b', through: 42 }).path).toBe('/api/today/report-changes?date=2026-10-03&cursor=a%2Fb&through_event_id=42');
    expect(reportEditsOperation('2026-10-03', 'a/b', 42, '20').path).toContain('track_id=a%2Fb&through_event_id=42&cursor=20');
    expect(dailyTrackOperation().responseSchema.safeParse([]).success).toBe(false);
  });
});
