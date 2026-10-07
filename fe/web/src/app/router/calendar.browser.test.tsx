import { render, cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { useState } from '../../ui/state/public.ts';
import { CalendarTasks, type CalendarEntriesView } from '../../features/calendar/public.tsx';
import type { CalendarEntry, CalendarListedEntry } from '../../../../core/domain/calendar.ts';

afterEach(async () => {
  cleanup();
  await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
});
const ready = (entries: CalendarListedEntry[]): CalendarEntriesView => ({ entries, loading: false, error: null });

it('queries the visible month and selected day separately through the real adapter', async () => {
  await page.viewport(1440, 1000);
  const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
  const { TodayCalendarTasks } = await import('./calendar.tsx');
  const { createUnauthorizedChannel } = await import('../../../../core/api/unauthorized.ts');
  const { calendarDraftSchema } = await import('../../../../core/domain/calendar.ts');
  type Request = import('../../../../core/api/types.ts').ApiRequest;
  const requests: Request[] = [];
  let visible: CalendarListedEntry[] = [];
  let saved: CalendarEntry | null = null;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const transport: import('../../../../core/api/types.ts').ApiTransportPort = {
    send(request) {
      requests.push(request);
      let body: unknown;
      if (request.path === '/api/plugins') body = [{ id: 'calendar', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Calendar', has_config: false, can_uninstall: false, can_disable: false }];
      else if (request.method === 'GET') body = visible;
      else if (request.path === '/api/calendar/tasks') {
        const payload = request.body as { task: unknown };
        saved = { id: 'browser-task', task: calendarDraftSchema.parse(payload.task), version: 1, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1 };
        visible = [{ ...saved, occurrences: [] }]; body = saved;
      } else { body = { ...saved, version: 2, cancelled: true }; visible = []; }
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  const unauthorized = createUnauthorizedChannel({ enqueue: (work) => work() });
  function Example() {
    const [date, setDate] = useState("2026-10-02");
    return <TodayCalendarTasks date={date} onDateChange={setDate} transport={transport} unauthorized={unauthorized} onSettings={() => undefined} onOpenTrack={() => undefined} />;
  }
  render(<QueryClientProvider client={client}><Example /></QueryClientProvider>);
  await expect.poll(() => requests.some((request) => request.path.includes('from=2026-09-28&until=2026-10-05'))).toBe(true);
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await page.getByRole('button', { name: 'Next month' }).click();
  await expect.poll(() => requests.some((request) => request.path.includes('from=2026-10-26&until=2026-12-07'))).toBe(true);
  await page.getByRole('button', { name: 'Previous month' }).click();
  await page.getByRole('link', { name: /October 4, 2026/ }).click();
  await expect.poll(() => requests.some((request) => request.path.includes('from=2026-10-04&until=2026-10-05'))).toBe(true);
  await page.getByRole('radio', { name: 'Week', exact: true }).click();
  await page.getByRole('button', { name: 'New task', exact: true }).click();
  await page.getByRole('textbox', { name: 'Task title' }).fill('Browser commitment');
  await userEvent.keyboard('{Enter}');
  await page.getByRole('button', { name: /Browser commitment.*All day/ }).click();
  await page.getByRole('button', { name: 'Cancel task', exact: true }).click();
  await expect.poll(() => requests.filter((request) => request.method === 'POST').length).toBe(2);
  const writes = requests.filter((request) => request.method === 'POST');
  expect(writes[0].body).toMatchObject({ task: { title: 'Browser commitment', schedule: { kind: 'all_day', date: '2026-10-04' } } });
  expect(writes[1].body).toMatchObject({ expected_version: 1, cancelled: true });
  await expect.element(page.getByRole('button', { name: /Browser commitment.*All day/ })).not.toBeInTheDocument();
  expect(requests.length).toBeLessThan(30);
  client.clear();
});

it('shows counts in both views and keeps task titles and times in the list', async () => {
  await page.viewport(1440, 1000);
  const span = { start: '2026-10-02T14:00:00+08:00', end: '2026-10-02T15:00:00+08:00' };
  const entry: CalendarListedEntry = { id: 'same-entry', task: { title: 'Review approach', description: 'Compare the options.', schedule: {
    kind: 'timed', ...span, timezone: 'Asia/Shanghai',
  } }, version: 1, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1, occurrences: [span] };
  render(<CalendarTasks date="2026-10-02" timezone="Asia/Shanghai" trackCountOn={(date) => date === '2026-10-02' ? 3 : 0} enabled pending={false}
    onDateChange={() => undefined} onWindowChange={() => undefined} onRetry={() => undefined} onSettings={() => undefined}
    onOpenTrack={() => undefined} onSave={() => Promise.resolve()} month={ready([entry])} day={ready([entry])} />);
  await expect.element(page.getByLabelText('1 tasks', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('button', { name: /^Review approach,/ })).not.toBeInTheDocument();
  await expect.element(page.getByRole('link', { name: /October 2, 2026.*3 tracks, 1 tasks/ })).toBeVisible();
  const task = page.getByRole('region', { name: 'Task list' }).getByRole('button', { name: /Review approach.*14:00/ });
  await expect.element(task).toBeVisible();
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  await expect.element(page.getByLabelText('1 tasks', { exact: true })).toBeVisible();
  await expect.element(page.getByRole('link', { name: /October 2, 2026.*3 tracks, 1 tasks/ })).toBeVisible();
  await task.click();
  await expect.element(page.getByRole('textbox', { name: 'Task title' })).toHaveValue('Review approach');
});
it('counts a task ending just after midnight on the next day', async () => {
  await page.viewport(1440, 1000);
  const span = { start: '2026-10-02T23:00:00+08:00', end: '2026-10-03T00:00:00.000001+08:00' };
  const entry: CalendarListedEntry = { id: 'precision', version: 1, cancelled: false, source_track_id: null, created_by: 'agent', created_at: 1, updated_at: 1,
    task: { title: 'Midnight review', description: '', schedule: { kind: 'timed', ...span, timezone: 'Asia/Shanghai' } }, occurrences: [span] };
  const props = { date: '2026-10-02', timezone: 'Asia/Shanghai', enabled: true, pending: false, onDateChange: () => undefined,
    onWindowChange: () => undefined, onRetry: () => undefined, onSettings: () => undefined, onOpenTrack: () => undefined, onSave: () => Promise.resolve() };
  const view = render(<CalendarTasks {...props} month={ready([entry])} day={ready([entry])} />);
  await expect.element(page.getByRole('link', { name: /October 3, 2026, 1 tasks/ })).toBeVisible();
  const exactSpan = { ...span, end: '2026-10-03T00:00:00+08:00' };
  const exact: CalendarListedEntry = { ...entry, task: { ...entry.task, schedule: { kind: 'timed', ...exactSpan, timezone: 'Asia/Shanghai' } }, occurrences: [exactSpan] };
  view.rerender(<CalendarTasks {...props} month={ready([exact])} day={ready([exact])} />);
  await expect.element(page.getByRole('link', { name: /October 3, 2026, 0 tasks/ })).toBeVisible();
});
it('counts a weekly entry on each projected occurrence in Week and Month and shows it read-only', async () => {
  await page.viewport(1440, 1000);
  const at = (day: string) => ({ start: `2026-10-${day}T09:30:00+08:00`, end: `2026-10-${day}T10:00:00+08:00` });
  const weekly: CalendarListedEntry = { id: 'weekly', version: 1, cancelled: false, source_track_id: 'track', created_by: 'card:planner', created_at: 1, updated_at: 1,
    task: { title: 'Open review', description: 'Check the overnight moves.', schedule: { kind: 'weekly', weekdays: ['mon', 'wed', 'fri'], start: '09:30', end: '10:00', timezone: 'Asia/Shanghai', from: '2026-10-01' } },
    occurrences: ['02', '05', '07', '09', '12', '14', '16', '19', '21', '23', '26', '28', '30'].map(at) };
  const today: CalendarListedEntry = { ...weekly, occurrences: [at('02')] };
  render(<CalendarTasks date="2026-10-02" timezone="Asia/Shanghai" enabled pending={false} onDateChange={() => undefined} onWindowChange={() => undefined}
    onRetry={() => undefined} onSettings={() => undefined} onOpenTrack={() => undefined} onSave={() => Promise.resolve()} month={ready([weekly])} day={ready([today])} />);
  // Wednesday September 30 precedes the series' first date.
  await expect.element(page.getByRole('link', { name: /September 30, 2026, 0 tasks/ })).toBeVisible();
  await expect.element(page.getByRole('link', { name: /October 2, 2026, selected, 1 tasks/ })).toBeVisible();
  await expect.element(page.getByRole('link', { name: /October 3, 2026, 0 tasks/ })).toBeVisible();
  await expect.element(page.getByRole('region', { name: 'Task list' }).getByRole('button', { name: /Open review.*09:30 – 10:00 · Weekly/ })).toBeVisible();
  await page.screenshot({ path: '../../../../test-results/calendar-weekly-week.png' });
  await page.getByRole('radio', { name: 'Month', exact: true }).click();
  for (const day of [5, 7, 9, 30]) await expect.element(page.getByRole('link', { name: new RegExp(`October ${day}, 2026, 1 tasks`) })).toBeVisible();
  for (const day of [6, 8, 31]) await expect.element(page.getByRole('link', { name: new RegExp(`October ${day}, 2026, 0 tasks`) })).toBeVisible();
  await page.screenshot({ path: '../../../../test-results/calendar-weekly-month.png' });
  await page.getByRole('region', { name: 'Task list' }).getByRole('button', { name: /Open review/ }).click();
  await expect.element(page.getByRole('dialog', { name: 'Weekly task' }).getByText('Repeats weekly — edit it through the Track.')).toBeVisible();
  await expect.element(page.getByRole('textbox', { name: 'Task title' })).not.toBeInTheDocument();
  await page.screenshot({ path: '../../../../test-results/calendar-weekly-details.png' });
});
