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
  it('shows the cove name and a pluralised wave count', () => {
    renderPage();
    expect(screen.getByRole('button', { name: 'Rename cove' }).textContent).toBe('Work');
    expect(screen.getByText('2 waves')).toBeTruthy();
  });

  it('uses the singular noun for one wave', () => {
    renderPage({ waveCount: 1 });
    expect(screen.getByText('1 wave')).toBeTruthy();
  });

  it('asks the caller to open the new-wave surface', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: '+ New wave' }));
    expect(props.onRequestNewWave).toHaveBeenCalledTimes(1);
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
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    expect(screen.getByText('The cove and every wave inside it are removed. This cannot be undone.')).toBeTruthy();
    expect(props.onDeleteCove).not.toHaveBeenCalled();
  });

  it('deletes once the confirm is accepted', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));
    expect(props.onDeleteCove).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('closes without deleting when the confirm is cancelled', async () => {
    const { props } = renderPage();
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onDeleteCove).not.toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
