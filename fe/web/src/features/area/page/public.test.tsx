// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../../core/domain/area.ts';
import { AreaPage } from './public.tsx';

afterEach(cleanup);

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function renderPage(overrides: Partial<Parameters<typeof AreaPage>[0]> = {}) {
  const props = {
    area: area(),
    trackCount: 2,
    trackList: <div>track list slot</div>,
    onRenameArea: vi.fn(),
    onDeleteArea: vi.fn(),
    onRequestNewTrack: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<AreaPage {...props} />) };
}

describe('AreaPage header', () => {
  it('shows the area name, and nothing else', () => {
    const { container } = renderPage();
    expect(screen.getByRole('button', { name: 'Rename area' }).textContent).toBe('Work');
    // No track count. It answered a question nobody asks — you open an area to
    // pick a track, not to learn how many there are — and the list below already
    // says it, at a glance, with the names attached. No identity dot either: it
    // was the only colour on the page and it restated the name beside it.
    expect(container.textContent).not.toMatch(/\d+ tracks?/);
  });

  // `trackCount` survives as a prop because the *confirm copy* spends it
  // ("This deletes 2 tracks"), which is the one place the number changes a
  // decision. See the delete suite below.
  it('asks the caller to open the new-track surface', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'New track' }));
    expect(props.onRequestNewTrack).toHaveBeenCalledTimes(1);
  });

  // §4.4's "一次性动作必须带文字" is overridden here for two glyphs in their
  // conventional meanings — but the tooltip may never stand in for the
  // accessible name, so both are present, not either.
  it('gives each header icon a tooltip as well as an accessible name', () => {
    renderPage();
    expect(screen.getByRole('button', { name: 'New track' }).getAttribute('title')).toBe('New track');
    expect(screen.getByRole('button', { name: 'Delete area Work' }).getAttribute('title')).toBe('Delete area');
  });

  it('renders the track list slot rather than its own list', () => {
    renderPage({ trackList: <p>ten tracks live here</p> });
    expect(screen.getByText('ten tracks live here')).toBeTruthy();
  });

  it('leaves the empty state to the list slot', () => {
    renderPage({ trackCount: 0, trackList: <p>No tracks yet.</p> });
    expect(screen.getAllByText(/No tracks yet\./)).toHaveLength(1);
  });
});

describe('AreaPage delete', () => {
  it('only opens the confirm; it does not delete on the header button', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    // §6.13's body is two sentences with different typography: what it costs,
    // then what to type. The count lands here, where it changes a decision.
    expect(screen.getByText('This deletes 2 tracks. This cannot be undone.')).toBeTruthy();
    expect(props.onDeleteArea).not.toHaveBeenCalled();
  });

  it('deletes once the name is typed and the confirm is accepted', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(props.onDeleteArea).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('refuses to delete while the typed name does not match', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    // Case-sensitive, and no Unicode normalisation: the point of a typed
    // confirm is that you reproduced the name, not that you approximated it.
    expect(props.onDeleteArea).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  it('closes without deleting when the confirm is cancelled', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onDeleteArea).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
