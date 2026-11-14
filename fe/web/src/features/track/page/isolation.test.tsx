import { cleanup, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

it('shows the warning in the header only for explicit none isolation', async () => {
  const view = renderPage({ track: { ...track(), isolation: 'none' } });
  expect(screen.getAllByText('Not isolated · no guarantees').length).toBeGreaterThan(0);
  await userEvent.click(screen.getAllByRole('button', { name: 'Not isolated · no guarantees' })[0]);
  const explanation = within(await screen.findByRole('dialog'));
  expect(explanation.getByText(/The Planner works directly in this folder/)).toBeTruthy();
  expect(explanation.getByText(/In a Git folder, dispatching a task worker automatically upgrades this Track to an isolated worktree while preserving the existing Planner folder/)).toBeTruthy();
  expect(explanation.getByText(/Non-Git folders remain unisolated and cannot start task workers/)).toBeTruthy();
  view.unmount();
  renderPage({ track: { ...track(), isolation: 'worktree' } });
  expect(screen.queryByText('Not isolated · no guarantees')).toBeNull();
});
