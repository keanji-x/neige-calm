import { cleanup, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

it('shows the warning in the header only for explicit none isolation', () => {
  const view = renderPage({ track: { ...track(), isolation: 'none' } });
  expect(screen.getAllByText('Not isolated · no guarantees').length).toBeGreaterThan(0);
  view.unmount();
  renderPage({ track: { ...track(), isolation: 'worktree' } });
  expect(screen.queryByText('Not isolated · no guarantees')).toBeNull();
});
