import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { CalendarTasks, type CalendarTasksProps } from './public.tsx';
import { loadCalendarGrid } from './grid-loader.ts';

// A failed native import can stay rejected for the document's lifetime. Never
// model recovery by making a second invocation magically resolve this module.
vi.mock(import('./grid-loader.ts'), () => ({ loadCalendarGrid: vi.fn(() => Promise.reject(new Error('Module unavailable'))) }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });
function props(): CalendarTasksProps {
  return { date: '2026-10-02', timezone: 'Asia/Shanghai', enabled: true, pending: false,
    month: { entries: [], loading: false, error: null }, day: { entries: [], loading: false, error: null },
    onDateChange: vi.fn(), onWindowChange: vi.fn(), onRetry: vi.fn(), onSettings: vi.fn(),
    onOpenTrack: vi.fn(), onSave: vi.fn(() => Promise.resolve()), onReload: vi.fn() };
}

it('offers an explicit page reload after a persistent module failure instead of another import', async () => {
  const input = props(); render(<CalendarTasks {...input} />);
  const reload = await screen.findByRole('button', { name: 'Reload page' });
  expect(screen.getByText(/Reloading clears unsaved changes/)).toBeDefined();
  expect(screen.queryByRole('button', { name: 'Retry calendar grid' })).toBeNull();
  await userEvent.click(reload);
  expect(input.onReload).toHaveBeenCalledOnce();
  expect(loadCalendarGrid).toHaveBeenCalledOnce();
});

it('keeps reload unavailable until the current task editor is finished', async () => {
  const input = props(); render(<CalendarTasks {...input} />);
  const reload = await screen.findByRole<HTMLButtonElement>('button', { name: 'Reload page' });
  await userEvent.click(screen.getByRole('button', { name: 'New task' }));
  await userEvent.type(screen.getByRole('textbox', { name: 'Task title' }), 'Keep these words');
  expect(reload.disabled).toBe(true);
  expect(input.onReload).not.toHaveBeenCalled();
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Task title' }).value).toBe('Keep these words');
  await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
  expect(reload.disabled).toBe(false);
});

it('does not reload while a calendar write is pending', async () => {
  const input = { ...props(), pending: true }; render(<CalendarTasks {...input} />);
  const reload = await screen.findByRole<HTMLButtonElement>('button', { name: 'Reload page' });
  expect(reload.disabled).toBe(true);
  await userEvent.click(reload);
  expect(input.onReload).not.toHaveBeenCalled();
});
