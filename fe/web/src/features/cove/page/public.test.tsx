// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { CovePage } from './public.tsx';

afterEach(cleanup);

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function renderPage(overrides: Partial<Parameters<typeof CovePage>[0]> = {}) {
  const props = {
    cove: cove(),
    waveCount: 2,
    waveList: <div>wave list slot</div>,
    onRenameCove: vi.fn(),
    onDeleteCove: vi.fn(),
    onRequestNewWave: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<CovePage {...props} />) };
}

describe('CovePage header', () => {
  it('shows the cove name, and nothing else', () => {
    const { container } = renderPage();
    expect(screen.getByRole('button', { name: 'Rename area' }).textContent).toBe('Work');
    // No wave count. It answered a question nobody asks — you open a cove to
    // pick a wave, not to learn how many there are — and the list below already
    // says it, at a glance, with the names attached. No identity dot either: it
    // was the only colour on the page and it restated the name beside it.
    expect(container.textContent).not.toMatch(/\d+ waves?/);
  });

  // `waveCount` survives as a prop because the *confirm copy* spends it
  // ("This deletes 2 waves"), which is the one place the number changes a
  // decision. See the delete suite below.
  it('asks the caller to open the new-wave surface', async () => {
    const { props } = renderPage();
    expect(screen.getByRole('heading', { name: 'Tracks' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'New track' }));
    expect(props.onRequestNewWave).toHaveBeenCalledTimes(1);
  });

  // §4.4's "一次性动作必须带文字" is overridden here for two glyphs in their
  // conventional meanings — but the tooltip may never stand in for the
  // accessible name, so both are present, not either.
  it('gives each header icon a tooltip as well as an accessible name', () => {
    renderPage();
    expect(screen.getByRole('button', { name: 'New track' }).getAttribute('title')).toBe('New track');
    expect(screen.getByRole('button', { name: 'Delete area Work' }).getAttribute('title')).toBe('Delete area');
  });

  it('renders the wave list slot rather than its own list', () => {
    renderPage({ waveList: <p>ten waves live here</p> });
    expect(screen.getByText('ten waves live here')).toBeTruthy();
  });

  it('leaves the empty state to the list slot', () => {
    renderPage({ waveCount: 0, waveList: <p>No waves yet.</p> });
    expect(screen.getAllByText(/No waves yet\./)).toHaveLength(1);
  });
});

describe('CovePage delete', () => {
  it('only opens the confirm; it does not delete on the header button', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    // §6.13's body is two sentences with different typography: what it costs,
    // then what to type. The count lands here, where it changes a decision.
    expect(screen.getByText('This deletes 2 tracks. This cannot be undone.')).toBeTruthy();
    expect(props.onDeleteCove).not.toHaveBeenCalled();
  });

  it('deletes once the name is typed and the confirm is accepted', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(props.onDeleteCove).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('refuses to delete while the typed name does not match', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    // Case-sensitive, and no Unicode normalisation: the point of a typed
    // confirm is that you reproduced the name, not that you approximated it.
    expect(props.onDeleteCove).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  it('closes without deleting when the confirm is cancelled', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onDeleteCove).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
