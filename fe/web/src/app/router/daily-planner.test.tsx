import { cleanup, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(cleanup);

it('uses the real Planner and opens cited reports from the daily homepage without creation writes', async () => {
  const { router, requests } = renderDailyFixture();
  await screen.findByText('Prioritize the release. Read', { exact: false });
  expect((await screen.findByRole('button', { name: 'Rename track' })).textContent).toBe('2026-10-04');
  expect(screen.queryByRole('navigation', { name: 'Daily Planner dates' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'Open daily Planner' })).toBeNull();
  expect(screen.queryByText('Earlier Today report')).toBeNull();
  const sidebar = screen.getByRole('navigation', { name: 'Workspace' });
  expect(within(sidebar).queryByText('2026-10-04')).toBeNull();
  expect(within(sidebar).queryByText('system')).toBeNull();
  await userEvent.click(await screen.findByRole('button', { name: 'Planner' }));
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
  await userEvent.click(await screen.findByText('Report changes'));
  const source = await screen.findByRole('heading', { name: /Project evidence.*2 report edits/ });
  expect(requests.some((request) => request.path.startsWith('/api/today/report-edits'))).toBe(false);
  await userEvent.click(source);
  expect((screen.getByText('Body changes').closest('details') as HTMLDetailsElement).open).toBe(false);
  await screen.findByText('Previous summary');
  await userEvent.click(screen.getByText('Individual edits'));
  await userEvent.click(await screen.findByRole('heading', { name: /Report edit.*09:00:00/ }));
  await userEvent.click(screen.getByText('After', { exact: true }));
  await screen.findByRole('heading', { name: 'After release' });
  expect(requests.some((request) => request.path === '/api/today/report-edits?date=2026-10-03&track_id=project&through_event_id=42')).toBe(true);
});

it('keeps report history failures distinct from no changes', async () => {
  renderDailyFixture({ failChanges: true });
  await userEvent.click(await screen.findByText('Report changes'));
  await screen.findByText('Report history unavailable');
  expect(screen.queryByText('No report changes recorded for visible Tracks on this day.')).toBeNull();
});

it('keeps historical daily links readable without a second date toolbar', async () => {
  renderDailyFixture({ initial: '/?day=2026-10-03' });
  await screen.findByText('No daily Track was created for this date.');
});
