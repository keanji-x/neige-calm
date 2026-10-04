import { cleanup, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(cleanup);

it('uses the real Planner and opens cited reports from the daily homepage without creation writes', async () => {
  const { router, requests } = renderDailyFixture();
  await screen.findByText('Prioritize the release. Read', { exact: false });
  const sidebar = screen.getByRole('navigation', { name: 'Workspace' });
  expect(within(sidebar).queryByText('2026-10-04')).toBeNull();
  expect(within(sidebar).queryByText('system')).toBeNull();
  await userEvent.click(await screen.findByRole('button', { name: 'Open daily Planner' }));
  await screen.findByRole('complementary', { name: 'Daily Planner conversation' });
  expect(requests.some((request) => request.path.endsWith('/planner/run'))).toBe(true);
  expect(requests.every((request) => request.method === 'GET')).toBe(true);
  await userEvent.keyboard('{Escape}');
  await userEvent.click(await screen.findByRole('button', { name: 'Project evidence' }));
  await waitFor(() => expect(router.state.location.pathname).toBe('/track/project'));
  await screen.findByText('Release is ready.');
});

it('reads yesterday’s report changes and individual edits with the server snapshot cursor', async () => {
  const { requests } = renderDailyFixture();
  await userEvent.click(await screen.findByText('Report changes · 2026-10-03'));
  await screen.findByText('2 report edits');
  await userEvent.click(screen.getByText('Individual edits'));
  await userEvent.click(await screen.findByText('Report edit · 09:00:00'));
  await screen.findByRole('heading', { name: 'After release' });
  expect(requests.some((request) => request.path === '/api/today/report-edits?date=2026-10-03&track_id=project&through_event_id=42')).toBe(true);
});

it('keeps missing historical days and history failures distinct', async () => {
  const { router } = renderDailyFixture({ failChanges: true });
  await userEvent.click(await screen.findByText('Report changes · 2026-10-03'));
  await screen.findByText('Report history unavailable');
  expect(screen.queryByText('No report changes recorded for visible Tracks on this day.')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Previous day' }));
  await waitFor(() => expect(router.state.location.searchStr).toBe('?day=2026-10-03'));
  await screen.findByText('No daily Track was created for this date.');
});
