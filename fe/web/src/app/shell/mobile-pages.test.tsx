// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { MobilePages } from './mobile-pages.tsx';

afterEach(cleanup);

const area: Area = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user',
  defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
};
const track = (overrides: Partial<Track>): Track => ({
  id: 'w1', areaId: 'c1', title: 'Recent report', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 10,
  ...NEUTRAL_ACTIVITY,
  ...overrides,
});

describe('MobilePages', () => {
  it('groups pinned Pages before recently updated Pages and opens the Report', async () => {
    const onOpenTrack = vi.fn(); const onBack = vi.fn(); const onOpenSettings = vi.fn();
    render(<MobilePages
      onBack={onBack}
      onNewTrack={vi.fn()}
      onCreateArea={vi.fn()}
      onEditArea={vi.fn()}
      onOpenSettings={onOpenSettings}
      areaId="c1"
      areas={[area]}
      tracks={[
        track({ id: 'recent', title: 'Recent report', updatedAt: 20 }),
        track({ id: 'pinned', title: 'Pinned report', pinnedAt: 30 }),
      ]}
      onOpenTrack={onOpenTrack}
    />);

    expect(screen.getByRole('radiogroup', { name: 'Page group' })).toBeTruthy();
    expect(screen.getByRole('radio', { name: 'Pinned' }).getAttribute('aria-checked')).toBe('true');
    expect(screen.queryByRole('button', { name: 'Recent report' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Pinned report' }));
    expect(onOpenTrack).toHaveBeenCalledWith('pinned');

    await userEvent.click(screen.getByRole('radio', { name: 'Recent' }));
    expect(screen.getByRole('button', { name: 'Recent report' })).toBeTruthy();
    const heading = screen.getByRole('heading', { name: 'Pages' });
    const settings = screen.getByRole('button', { name: 'Settings' });
    expect(settings.closest('header')).toBeNull();
    expect(heading.closest('header')?.nextElementSibling?.contains(settings)).toBe(true);
    expect(settings.textContent?.trim()).toBe('Settings');
    await userEvent.click(settings);
    await userEvent.click(screen.getByRole('button', { name: 'Back to workspace' }));
    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(onBack).toHaveBeenCalledOnce();
  });
});
