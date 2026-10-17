import '../../styles/entry.css';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import { useState } from '../../ui/state/public.ts';
import { TodayCalendarTasks } from './calendar.tsx';
import { loadCalendarGrid } from '../../features/calendar/grid-loader.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { calendarDraftSchema } from '../../../../core/domain/calendar.ts';
import type { ApiRequest, ApiTransportPort } from '../../../../core/api/types.ts';

vi.mock(import('../../features/calendar/grid-loader.ts'), () => ({ loadCalendarGrid: vi.fn() }));
function deferred() {
  let resolve!: (value: Awaited<ReturnType<typeof loadCalendarGrid>>) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<Awaited<ReturnType<typeof loadCalendarGrid>>>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
beforeEach(async () => { await page.viewport(1400, 900); });
afterEach(() => { cleanup(); vi.resetAllMocks(); });
function mount(enabled = true) {
  const requests: ApiRequest[] = [];
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const transport: ApiTransportPort = { send: request => {
    requests.push(request);
    if (request.method === 'POST') return Promise.resolve({ status: 200, statusText: 'OK', body: {
      id: 'created-task', task: calendarDraftSchema.parse((request.body as { task: unknown }).task), version: 1, cancelled: false,
      source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1,
    } });
    return Promise.resolve({ status: 200, statusText: 'OK', body: request.path === '/api/plugins'
      ? [{ id: 'calendar', version: '0.1.0', enabled, state: enabled ? 'running' : 'disabled', manifest_name: 'Calendar', has_config: false, can_uninstall: false, can_disable: false }]
      : [] });
  } };
  const unauthorized = createUnauthorizedChannel({ enqueue: work => work() });
  let changeDate!: (date: string) => void;
  function Scene() {
    const [date, setDate] = useState('2026-10-02'); changeDate = setDate;
    return <><button type="button">Outside choice</button><TodayCalendarTasks date={date} onDateChange={setDate}
      transport={transport} unauthorized={unauthorized} onSettings={() => {}} onOpenTrack={() => {}} /></>;
  }
  const view = render(<QueryClientProvider client={client}><Scene /></QueryClientProvider>);
  return { ...view, client, requests, changeDate: (date: string) => changeDate(date) };
}

it('keeps day tasks and the live editor available while the actual grid module waits', async () => {
  const loading = deferred(); vi.mocked(loadCalendarGrid).mockReturnValue(loading.promise);
  const view = mount();
  try {
    await screen.findByText('No tasks for this day.');
    expect(screen.getAllByText('Loading calendar…')).toHaveLength(1);
    expect(view.requests.filter(request => request.path.startsWith('/api/calendar/tasks'))).toHaveLength(1);
    await page.getByRole('radio', { name: 'Month', exact: true }).click();
    await page.getByRole('button', { name: 'New task', exact: true }).click();
    await page.getByRole('textbox', { name: 'Task title' }).fill('Keep this draft');
    const title = screen.getByRole<HTMLInputElement>('textbox', { name: 'Task title' });
    title.setSelectionRange(2, 7);
    act(() => { view.changeDate('2026-11-03'); });
    await act(async () => { loading.resolve(await import('../../features/calendar/calendar-view.tsx')); });
    await waitFor(() => expect(screen.queryByText('Loading calendar…')).toBeNull());
    expect(screen.getByRole('textbox', { name: 'Task title' })).toBe(title);
    expect(title.value).toBe('Keep this draft');
    expect(document.activeElement).toBe(title);
    expect([title.selectionStart, title.selectionEnd]).toEqual([2, 7]);
    await page.getByRole('button', { name: 'Create task', exact: true }).click();
    await waitFor(() => expect(view.requests.some(request => request.method === 'POST')).toBe(true));
    expect(view.requests.find(request => request.method === 'POST')?.body).toMatchObject({ task: { title: 'Keep this draft', schedule: { kind: 'all_day', date: '2026-10-02' } } });
    await expect.element(page.getByRole('radio', { name: 'Month', exact: true })).toBeChecked();
    await expect.element(page.getByRole('link', { name: /November 3, 2026, selected/ })).toBeVisible();
    await expect.element(page.getByRole('link', { name: /November 30, 2026/ })).toBeVisible();
  } finally { cleanup(); view.client.clear(); }
});

it('retries only the failed grid while preserving disabled-plugin navigation and later focus', async () => {
  const first = deferred(); const second = deferred();
  vi.mocked(loadCalendarGrid).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const view = mount(false);
  try {
    await screen.findByText('Calendar is temporarily unavailable.');
    expect(screen.getByRole('button', { name: 'New task' }).hasAttribute('disabled')).toBe(true);
    expect(loadCalendarGrid).toHaveBeenCalledOnce();
    // The selected month request must survive both failed and delayed module attempts.
    await page.getByRole('radio', { name: 'Month', exact: true }).click();
    await act(async () => { first.reject(new Error('Chunk unavailable')); await Promise.resolve(); });
    await page.getByRole('button', { name: 'Retry calendar grid', exact: true }).click();
    expect(loadCalendarGrid).toHaveBeenCalledTimes(2);
    act(() => { view.changeDate('2026-11-03'); });
    const outside = screen.getByRole('button', { name: 'Outside choice' }); outside.focus();
    await act(async () => { second.resolve(await import('../../features/calendar/calendar-view.tsx')); });
    await expect.element(page.getByRole('link', { name: /November 3, 2026, selected/ })).toBeVisible();
    await expect.element(page.getByRole('link', { name: /November 30, 2026/ })).toBeVisible();
    expect(document.activeElement).toBe(outside);
    expect(view.requests.some(request => request.path.startsWith('/api/calendar/tasks'))).toBe(false);
    await page.getByRole('link', { name: /November 4, 2026/ }).click();
    await expect.element(page.getByRole('link', { name: /November 4, 2026, selected/ })).toBeVisible();
  } finally { cleanup(); view.client.clear(); }
});


it('keeps New task usable after a grid failure and ignores a late retry after leaving', async () => {
  const first = deferred(); const second = deferred();
  vi.mocked(loadCalendarGrid).mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
  const view = mount();
  try {
    await screen.findByText('No tasks for this day.');
    await act(async () => { first.reject(new Error('Module unavailable')); await Promise.resolve(); });
    expect(screen.queryByText('Loading calendar…')).toBeNull();
    await page.getByRole('button', { name: 'New task', exact: true }).click();
    await page.getByRole('textbox', { name: 'Task title' }).fill('The editor remains usable');
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Task title' }).value).toBe('The editor remains usable');
    await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    await page.getByRole('button', { name: 'Retry calendar grid', exact: true }).click();
    expect(loadCalendarGrid).toHaveBeenCalledTimes(2);
    view.unmount();
    render(<button type="button">New surface</button>);
    const next = screen.getByRole('button', { name: 'New surface' }); next.focus();
    await act(async () => { second.resolve(await import('../../features/calendar/calendar-view.tsx')); });
    expect(document.activeElement).toBe(next);
    expect(screen.queryByLabelText('Calendar tasks')).toBeNull();
    expect(view.requests.some(request => request.method === 'POST')).toBe(false);
  } finally { cleanup(); view.client.clear(); }
});
