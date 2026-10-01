import { render, cleanup } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Calendar } from '../../features/calendar/public.tsx';
import { TodayPage } from '../../features/today/public.tsx';
import type { CalendarWrite } from '../../../../core/domain/calendar.ts';

afterEach(cleanup);
for (const width of [1280]) {
  it(`creates a timed commitment through Today at ${width}px`, async () => {
    await page.viewport(width, 850);
    const writes: CalendarWrite[] = [];
    const calendar = <Calendar date="2026-10-02" timezone="Asia/Shanghai" entries={[]} enabled loading={false} error={null} pending={false}
      onDate={() => undefined} onRetry={() => undefined} onSettings={() => undefined} onOpenTrack={() => undefined} onSave={(write) => { writes.push(write); return Promise.resolve(); }} />;
    render(<TodayPage tracks={[]} areas={[]} activityAvailable renderTrackRow={() => null} calendar={calendar} nowMs={Date.parse('2026-10-02T09:00:00+08:00')} />);
    await page.screenshot({ path: `../../../../test-results/calendar-default-${width}.png` });
    await page.getByRole('textbox', { name: 'Task name', exact: true }).fill('Research options');
    await page.getByRole('button', { name: 'Set time' }).click();
    await page.getByRole('textbox', { name: 'Start', exact: true }).fill('14:00');
    await page.getByRole('textbox', { name: 'End', exact: true }).fill('16:00');
    await page.screenshot({ path: `../../../../test-results/calendar-${width}.png` });
    await page.getByRole('button', { name: 'Add task' }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].task.schedule).toEqual({ kind: 'timed', start: '2026-10-02T14:00:00+08:00', end: '2026-10-02T16:00:00+08:00', timezone: 'Asia/Shanghai' });
  });
}

it('connects quick creation and cancellation through the production query and mutation adapter', async () => {
  await page.viewport(1280, 850);
  const { QueryClient, QueryClientProvider } = await import('@tanstack/react-query');
  const { TodayCalendar } = await import('./calendar.tsx');
  const { createUnauthorizedChannel } = await import('../../../../core/api/unauthorized.ts');
  const { calendarDraftSchema } = await import('../../../../core/domain/calendar.ts');
  type Entry = import('../../../../core/domain/calendar.ts').CalendarEntry;
  type Request = import('../../../../core/api/types.ts').ApiRequest;
  const requests: Request[] = [];
  let visible: Entry[] = [];
  let saved: Entry | null = null;
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const transport: import('../../../../core/api/types.ts').ApiTransportPort = {
    send(request) {
      requests.push(request);
      let body: unknown;
      if (request.path === '/api/plugins') body = [{ id: 'dev.neige.calendar', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Calendar', has_config: false, can_uninstall: false }];
      else if (request.method === 'GET') body = visible;
      else if (request.path === '/api/calendar/tasks') {
        const payload = request.body as { task: unknown };
        saved = { id: 'browser-task', task: calendarDraftSchema.parse(payload.task), version: 1, cancelled: false, source_track_id: null, created_by: 'user', created_at: 1, updated_at: 1 };
        visible = [saved]; body = saved;
      } else { body = { ...saved, version: 2, cancelled: true }; visible = []; }
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  render(<QueryClientProvider client={client}><TodayCalendar transport={transport} unauthorized={createUnauthorizedChannel({ enqueue: (work) => work() })} onSettings={() => undefined} onOpenTrack={() => undefined} /></QueryClientProvider>);
  await page.getByRole('textbox', { name: 'Task name' }).fill('Browser commitment');
  await userEvent.keyboard('{Enter}');
  await page.getByRole('button', { name: 'Browser commitment', exact: true }).click();
  await page.getByRole('button', { name: 'Cancel task', exact: true }).click();
  await expect.poll(() => requests.filter((request) => request.method === 'POST').length).toBe(2);
  const writes = requests.filter((request) => request.method === 'POST');
  expect(writes.map((request) => request.path)).toEqual(['/api/calendar/tasks', '/api/calendar/tasks/browser-task']);
  expect(writes[1].body).toMatchObject({ expected_version: 1, cancelled: true });
  await expect.element(page.getByRole('button', { name: 'Browser commitment', exact: true })).not.toBeInTheDocument();
  client.clear();
});

it('does not mount the desktop calendar task surface in the compact viewport', async () => {
  await page.viewport(390, 844);
  const calendar = <section aria-label="Calendar tasks">Desktop calendar</section>;
  render(<TodayPage tracks={[]} areas={[]} activityAvailable renderTrackRow={() => null} calendar={calendar} />);
  await expect.element(page.getByRole('region', { name: 'Calendar tasks' })).not.toBeInTheDocument();
});
