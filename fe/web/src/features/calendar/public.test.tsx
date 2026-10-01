import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { Calendar, type CalendarProps } from './public.tsx';
import type { CalendarEntry } from '../../../../core/domain/calendar.ts';

afterEach(cleanup);
const entry: CalendarEntry = { id: 'one', task: { title: 'Research', description: 'Compare options', schedule: { kind: 'all_day', date: '2026-10-02' } }, version: 3, cancelled: false, source_track_id: 'source', created_by: 'agent', created_at: 1, updated_at: 1 };
function props(overrides: Partial<CalendarProps> = {}): CalendarProps {
  return { date: '2026-10-02', timezone: 'Asia/Shanghai', entries: [], enabled: true, loading: false, error: null, pending: false, onDate: vi.fn(), onRetry: vi.fn(), onSettings: vi.fn(), onOpenTrack: vi.fn(), onSave: vi.fn(() => Promise.resolve()), ...overrides };
}
it('preserves a creation key after response loss and does not show success', async () => {
  const onSave = vi.fn().mockRejectedValueOnce(new Error('Response lost')).mockResolvedValueOnce(undefined);
  render(<Calendar {...props({ onSave })} />);
  const user = userEvent.setup();
  expect(screen.queryByRole('textbox', { name: 'Time zone' })).toBeNull();
  expect(screen.queryByRole('textbox', { name: 'Start' })).toBeNull();
  await user.type(screen.getByRole('textbox', { name: 'Task name' }), 'Research');
  await user.click(screen.getByRole('button', { name: 'Add task' }));
  expect(screen.getByRole('alert').textContent).toContain('Response lost');
  await user.click(screen.getByRole('button', { name: 'Add task' }));
  expect(onSave.mock.calls[0][0]).toEqual(onSave.mock.calls[1][0]);
  expect(screen.queryByRole('dialog')).toBeNull();
});
it('lets a human edit or cancel an AI entry using its actual revision', async () => {
  const onSave = vi.fn(() => Promise.resolve());
  render(<Calendar {...props({ entries: [entry], onSave })} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'Research' }));
  await user.click(screen.getByRole('button', { name: 'Cancel task' }));
  expect(onSave).toHaveBeenCalledWith({ id: 'one', expected_version: 3, task: entry.task, cancelled: true });
});
it('keeps errors distinct from empty or disabled calendars', () => {
  render(<Calendar {...props({ error: 'Offline', enabled: false })} />);
  expect(screen.getByRole('alert').textContent).toContain('Offline');
  expect(screen.queryByRole('button', { name: 'Open settings' })).toBeNull();
  expect(screen.queryByRole('textbox', { name: 'Task name' })).toBeNull();
});
it('keeps the quick draft while selecting another day or loading its entries', async () => {
  const onSave = vi.fn(() => Promise.resolve());
  const view = render(<Calendar {...props({ onSave })} />);
  const user = userEvent.setup();
  await user.type(screen.getByRole('textbox', { name: 'Task name' }), 'Read the report');
  view.rerender(<Calendar {...props({ date: '2026-10-03', loading: true, entries: undefined, onSave })} />);
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Task name' }).value).toBe('Read the report');
  await user.click(screen.getByRole('button', { name: 'Add task' }));
  expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ task: { title: 'Read the report', description: '', schedule: { kind: 'all_day', date: '2026-10-03' } } }));
});
it('discloses an existing task timezone and cross-day end while editing', async () => {
  const timed: CalendarEntry = { ...entry, task: { ...entry.task, schedule: { kind: 'timed', start: '2026-10-02T23:00:00-04:00', end: '2026-10-03T01:00:00-04:00', timezone: 'America/New_York' } } };
  render(<Calendar {...props({ entries: [timed] })} />);
  await userEvent.setup().click(screen.getByRole('button', { name: 'Research' }));
  expect(screen.getByRole('dialog').textContent).toContain('Times in America/New_York');
  expect(screen.getByRole('combobox', { name: 'End date' })).toBeTruthy();
});
it('labels the date of a range endpoint outside the selected day', () => {
  const overnight: CalendarEntry = { ...entry, task: { ...entry.task, schedule: { kind: 'timed', start: '2026-10-01T23:00:00+08:00', end: '2026-10-02T01:00:00+08:00', timezone: 'Asia/Shanghai' } } };
  render(<Calendar {...props({ entries: [overnight] })} />);
  expect(screen.getByRole('listitem').textContent).toMatch(/Oct 1/);
});
