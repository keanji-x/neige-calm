import { cleanup, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';
import { NO_ISOLATION_LABEL } from '../../../../../core/domain/workspace-isolation.ts';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

it('shows the warning in the header only for none isolation', async () => {
  const view = renderPage({ track: { ...track(), isolation: 'none' } });
  expect(screen.getAllByText(NO_ISOLATION_LABEL).length).toBeGreaterThan(0);
  await userEvent.click(screen.getAllByRole('button', { name: NO_ISOLATION_LABEL })[0]);
  const explanation = within(await screen.findByRole('dialog'));
  expect(explanation.getByText(/The Planner works directly in this source folder/)).toBeTruthy();
  expect(explanation.getByText(/Task workers require an isolated worktree/)).toBeTruthy();
  view.unmount();
  renderPage({ track: { ...track(), isolation: 'worktree' } });
  expect(screen.queryByText(NO_ISOLATION_LABEL)).toBeNull();
});
