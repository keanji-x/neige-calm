import { notifyManager } from '@tanstack/react-query';
import { act, cleanup, fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { renderDailyFixture } from '../router/daily-planner-fixture.tsx';
import { sidebarTrackGroups } from './sidebar-track-groups.ts';

vi.mock(import('./sidebar-track-groups.ts'), async original => {
  const actual = await original();
  return { ...actual, sidebarTrackGroups: vi.fn(actual.sidebarTrackGroups) };
});

afterEach(() => { cleanup(); vi.clearAllMocks(); document.getElementById('root')?.remove(); });

it('keeps shared workspace memberships out of unrelated shell layout updates', async () => {
  const fixture = renderDailyFixture({ initial: '/track/project' });
  try {
    await screen.findByRole('heading', { name: 'Project result' }, { timeout: 5_000 });
    await waitFor(() => {
      expect(fixture.client.isFetching()).toBe(0);
      expect(fixture.client.isMutating()).toBe(0);
    });
    // Cache idle precedes queued observer notifications; commit them before measuring layout work.
    await act(async () => { await new Promise<void>(resolve => notifyManager.schedule(resolve)); });
    vi.mocked(sidebarTrackGroups).mockClear();
    fireEvent.click(screen.getByRole('button', { name: 'Collapse sidebar' }));
    await screen.findByRole('button', { name: 'Expand sidebar' });
    fireEvent.click(screen.getByRole('button', { name: 'Expand sidebar' }));
    await screen.findByRole('button', { name: 'Collapse sidebar' });
    expect(sidebarTrackGroups).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: /^Track Project evidence/ }).getAttribute('aria-current')).toBe('page');
  } finally { cleanup(); fixture.client.clear(); }
});
