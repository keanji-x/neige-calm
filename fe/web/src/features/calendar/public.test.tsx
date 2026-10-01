import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { CalendarTasks, type CalendarTasksProps } from './public.tsx';
import { CalendarEditor } from './editor.tsx';
import type { CalendarEntry } from '../../../../core/domain/calendar.ts';

afterEach(cleanup);
const entry: CalendarEntry = { id: 'one', task: { title: 'Research', description: 'Compare options', schedule: { kind: 'all_day', date: '2026-10-02' } }, version: 3, cancelled: false, source_track_id: 'source', created_by: 'agent', created_at: 1, updated_at: 1 };
function props(overrides: Partial<CalendarTasksProps> = {}): CalendarTasksProps {
  return { date: '2026-10-02', timezone: 'Asia/Shanghai', month: { entries: [], loading: false, error: null }, day: { entries: [], loading: false, error: null }, enabled: true, pending: false, onDateChange: vi.fn(), onWindowChange: vi.fn(), onRetry: vi.fn(), onSettings: vi.fn(), onOpenTrack: vi.fn(), onSave: vi.fn(() => Promise.resolve()), ...overrides };
}
it('opens a minimal form and preserves its creation key after response loss', async () => {
  const onSave = vi.fn().mockRejectedValueOnce(new Error('Response lost')).mockResolvedValueOnce(undefined);
  render(<CalendarTasks {...props({ onSave })} />);
  const user = userEvent.setup();
  expect(screen.queryByRole('textbox', { name: 'Task title' })).toBeNull();
  await user.click(screen.getByRole('button', { name: 'New task' }));
  expect(screen.queryByRole('textbox', { name: 'Time zone' })).toBeNull();
  expect(screen.queryByRole('textbox', { name: 'Start' })).toBeNull();
  expect(screen.queryByRole('textbox', { name: 'Notes' })).toBeNull();
  await user.type(screen.getByRole('textbox', { name: 'Task title' }), 'Research');
  await user.click(screen.getByRole('button', { name: 'Create task' }));
  expect(within(screen.getByRole('dialog')).getAllByRole('alert').map((alert) => alert.textContent).join(' ')).toContain('Response lost');
  await user.click(screen.getByRole('button', { name: 'Create task' }));
  expect(onSave.mock.calls[0][0]).toEqual(onSave.mock.calls[1][0]);
  expect(screen.queryByRole('dialog')).toBeNull();
});
it('lets a human cancel an AI entry using its actual revision', async () => {
  const onSave = vi.fn(() => Promise.resolve());
  render(<CalendarEditor entry={entry} date="2026-10-02" timezone="Asia/Shanghai" pending={false} onClose={() => undefined} onSave={onSave} />);
  const user = userEvent.setup();
  await user.clear(screen.getByRole('textbox', { name: 'Task title' }));
  await user.click(screen.getByRole('button', { name: 'Cancel task' }));
  expect(onSave).toHaveBeenCalledWith({ id: 'one', expected_version: 3, task: entry.task, cancelled: true });
});
it('keeps errors distinct from empty or disabled calendars', () => {
  render(<CalendarTasks {...props({ month: { entries: undefined, loading: false, error: 'Offline' }, enabled: false })} />);
  expect(screen.getByRole('alert').textContent).toContain('Offline');
  expect(screen.queryByRole('button', { name: 'Enable Calendar in Settings' })).toBeNull();
  expect(screen.getByRole('button', { name: 'New task' }).hasAttribute('disabled')).toBe(true);
});
it('retains the open dialog date and draft when the background date changes', async () => {
  const onSave = vi.fn(() => Promise.resolve());
  const view = render(<CalendarTasks {...props({ onSave })} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole('button', { name: 'New task' }));
  await user.type(screen.getByRole('textbox', { name: 'Task title' }), 'Read the report');
  view.rerender(<CalendarTasks {...props({ date: '2026-10-03', month: { entries: undefined, loading: true, error: null }, day: { entries: undefined, loading: true, error: null }, onSave })} />);
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Task title' }).value).toBe('Read the report');
  await user.click(screen.getByRole('button', { name: 'Create task' }));
  expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ task: { title: 'Read the report', description: '', schedule: { kind: 'all_day', date: '2026-10-02' } } }));
});
it('discloses an existing task timezone and cross-day end while editing', () => {
  const timed: CalendarEntry = { ...entry, task: { ...entry.task, schedule: { kind: 'timed', start: '2026-10-02T23:00:00-04:00', end: '2026-10-03T01:00:00-04:00', timezone: 'America/New_York' } } };
  render(<CalendarEditor entry={timed} date="2026-10-02" timezone="Asia/Shanghai" pending={false} onClose={() => undefined} onSave={() => Promise.resolve()} />);
  expect(screen.getByText('Times in America/New_York')).toBeTruthy();
  expect(screen.getByRole('combobox', { name: 'End date' })).toBeTruthy();
});
it('preserves the offset of an ambiguous existing time when changing the title', async () => {
  const task = { ...entry.task, schedule: { kind: 'timed' as const, start: '2026-11-01T01:30:00-04:00', end: '2026-11-01T01:30:00-05:00', timezone: 'America/New_York' } };
  const onSave = vi.fn(() => Promise.resolve());
  render(<CalendarEditor entry={{ ...entry, task }} date="2026-11-01" timezone="America/New_York" pending={false} onClose={() => undefined} onSave={onSave} />);
  const user = userEvent.setup();
  await user.type(screen.getByRole('textbox', { name: 'Task title' }), ' updated');
  await user.click(screen.getByRole('button', { name: 'Save changes' }));
  expect(onSave).toHaveBeenCalledWith(expect.objectContaining({ task: { ...task, title: 'Research updated' } }));
});
it('orders daily tasks all-day first then by start time, independent of creation order', () => {
  const timed = (id: string, hour: string): CalendarEntry => ({ ...entry, id, task: { ...entry.task, title: id, schedule: { kind: 'timed', start: `2026-10-02T${hour}:00:00+08:00`, end: `2026-10-02T${hour}:30:00+08:00`, timezone: 'Asia/Shanghai' } } });
  render(<CalendarTasks {...props({ day: { entries: [timed('Evening', '17'), timed('Morning', '09'), entry], loading: false, error: null } })} />);
  const buttons = within(screen.getByRole('region', { name: 'Selected day tasks' })).getAllByRole('button');
  expect(buttons.map((button) => button.textContent)).toEqual(['', 'ResearchAll day', 'Morning09:00 – 09:30', 'Evening17:00 – 17:30']);
});
