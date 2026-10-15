// @vitest-environment jsdom
// The Track page shows the server's Area admin grant as a badge; the role itself is picked in the Planner
// conversation's composer, so the Track actions offer nothing for it.
import { cleanup, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { renderPage } from './test-fixtures.tsx';

afterEach(cleanup);

describe('TrackPage Area admin', () => {
  it('shows no Admin badge without a grant', () => {
    renderPage({ adminGrantedAt: null });
    expect(screen.queryAllByRole('status', { name: 'Area admin' })).toHaveLength(0);
  });

  it('shows the Admin badge from the server grant and offers no Admin action among the Track actions', async () => {
    renderPage({ adminGrantedAt: 7 });
    expect(screen.getAllByRole('status', { name: 'Area admin' }).map((badge) => badge.textContent)).toEqual(['Admin', 'Admin']);
    await userEvent.click(screen.getByRole('button', { name: /^Track actions for / }));
    expect(screen.getByRole('menuitem', { name: 'Delete track' })).toBeTruthy();
    expect(screen.queryByRole('menuitem', { name: /admin/i })).toBeNull();
  });
});
