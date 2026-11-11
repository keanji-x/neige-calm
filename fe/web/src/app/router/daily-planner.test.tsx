import { cleanup, screen, waitFor, within, fireEvent, act } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

afterEach(cleanup);

it('uses the real Planner and opens cited reports from the daily homepage without creation writes', async () => {
  const { router, requests } = renderDailyFixture({ initial: '/?day=2026-10-04' });
  // Functional content waits include admission; production latency is measured separately.
  await screen.findByText('Prioritize the release. Read', { exact: false }, { timeout: 5_000 });
  expect((await screen.findByRole('button', { name: 'Rename track' })).textContent).toBe('2026-10-04');
  expect(screen.queryByRole('navigation', { name: 'Daily Planner dates' })).toBeNull();
  expect(screen.queryByRole('button', { name: 'Open daily Planner' })).toBeNull();
  expect(screen.queryByText('Earlier Today report')).toBeNull();
  const sidebar = screen.getByRole('navigation', { name: 'Workspace' });
  expect(within(sidebar).queryByText('2026-10-04')).toBeNull();
  expect(within(sidebar).queryByText('system')).toBeNull();
  await userEvent.click(await screen.findByRole('button', { name: '讨论这项工作' }));
  await screen.findByRole('complementary', { name: 'Daily Planner conversation' });
  expect(requests.some((request) => request.path.includes('/planner/run?contract=conversation_v1'))).toBe(true);
  expect(requests.every((request) => request.method === 'GET')).toBe(true);
  await userEvent.keyboard('{Escape}');
  await userEvent.click(await screen.findByRole('button', { name: 'Project evidence' }));
  await waitFor(() => expect(router.state.location.pathname).toBe('/track/project'));
  await screen.findByText('Release is ready.', {}, { timeout: 5_000 });
});

it('reads yesterday’s report changes and individual edits with the server snapshot cursor', async () => {
  const { requests } = renderDailyFixture({ initial: '/?day=2026-10-04' });
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
  await userEvent.click(screen.getByRole('button', { name: 'Back to Today' }));
  expect((screen.getByText('Report changes').closest('details') as HTMLDetailsElement).open).toBe(false);
});

it('keeps report history failures distinct from no changes', async () => {
  renderDailyFixture({ failChanges: true });
  await userEvent.click(await screen.findByText('Report changes'));
  /* A 500's text is the server's internals: only the fixed sentence shows. */
  await screen.findByText('Report changes could not be loaded.');
  expect(screen.queryByText(/Report history unavailable/)).toBeNull();
  expect(screen.queryByText('No report changes recorded for visible Tracks on this day.')).toBeNull();
});

it('keeps historical daily links readable without a second date toolbar', async () => {
  renderDailyFixture({ initial: '/?day=2026-10-03' });
  await screen.findByText('No daily Track was created for this date.');
});


it.each(['/today/legacy', '/today/legacy?day=2026-10-04'])('redirects %s to the sole Today renderer without singleton requests', async (initial) => {
  const { router, requests } = renderDailyFixture({ initial });
  if (initial.includes('?')) await screen.findByText('Prioritize the release.', { exact: false }, { timeout: 5_000 });
  else await screen.findByRole('region', { name: 'Today Assistant' });
  expect(router.state.location.pathname).toBe('/');
  await screen.findByRole('region', { name: 'Calendar tasks' });
  expect(requests.some((request) => request.path.includes('/today/launchpad'))).toBe(false);
  if (initial.includes('?')) expect(router.state.location.search).toEqual({ day: '2026-10-04' });
});

it('opens Today Assistant in the center and returns from report changes with its draft', async () => {
  renderDailyFixture();
  const assistant = await screen.findByRole('region', { name: 'Today Assistant' });
  const field = await within(assistant).findByRole('combobox', { name: 'Message' });
  await userEvent.click(field);
  await userEvent.type(field, 'Keep this daily draft');
  await userEvent.click(screen.getByRole('button', { name: 'Report changes' }));
  await screen.findByRole('button', { name: 'Back to Today Assistant' });
  expect(screen.queryByRole('region', { name: 'Today Assistant' })).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Back to Today Assistant' }));
  expect(within(screen.getByRole('region', { name: 'Today Assistant' })).getByRole('combobox', { name: 'Message' }).textContent).toContain('Keep this daily draft');
  expect(screen.getByRole('region', { name: 'Calendar tasks' })).toBeTruthy();
});


it('sends from Today through the existing Planner endpoint and shows a refused send', async () => {
  const { requests } = renderDailyFixture({ reply: request => request.path.endsWith('/planner/input')
    ? { status: 403, statusText: 'Forbidden', body: { error: 'forbidden' } } : undefined });
  const assistant = await screen.findByRole('region', { name: 'Today Assistant' });
  const field = await within(assistant).findByRole('combobox', { name: 'Message' });
  field.textContent = 'Plan the release';
  const range = document.createRange();
  range.selectNodeContents(field);
  range.collapse(false);
  window.getSelection()!.removeAllRanges();
  window.getSelection()!.addRange(range);
  await act(async () => { fireEvent.input(field); await Promise.resolve(); });
  const send = within(assistant).getByRole('button', { name: 'Send' });
  await waitFor(() => expect(send.getAttribute('aria-disabled')).not.toBe('true'));
  await waitFor(() => expect(send.hasAttribute('disabled')).toBe(false));
  await userEvent.click(send);
  await waitFor(() => expect(requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1));
  await within(assistant).findByText(/^Not sent\./);
  const input = requests.filter(request => request.path.endsWith('/planner/input'));
  expect(input).toHaveLength(1);
  expect(input[0].path).toBe('/api/cards/daily-planner/planner/input');
  expect(input[0].method).toBe('POST');
});

it('shows the existing history retry in Today when conversation history cannot load', async () => {
  renderDailyFixture({ reply: request => request.path.includes('/harness/items')
    ? { status: 500, statusText: 'Error', body: { error: 'unavailable' } } : undefined });
  const assistant = await screen.findByRole('region', { name: 'Today Assistant' });
  await within(assistant).findByRole('button', { name: 'Reload history' });
});


it('returns to the center conversation when selected from the right inventory', async () => {
  renderDailyFixture();
  await screen.findByRole('region', { name: 'Today Assistant' });
  await userEvent.click(screen.getByRole('button', { name: 'Report changes' }));
  await screen.findByRole('button', { name: 'Back to Today Assistant' });
  await userEvent.click(screen.getByRole('button', { name: /Conversation Daily Planner conversation/ }));
  await screen.findByRole('region', { name: 'Today Assistant' });
  expect(screen.queryByRole('button', { name: 'Back to Today Assistant' })).toBeNull();
  expect(screen.getByRole('button', { name: 'New conversation' })).toBeTruthy();
});
