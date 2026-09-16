// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { MobileTracks } from './mobile-tracks.tsx';

afterEach(cleanup);

const area: Area = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user',
  defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
};
const track: Track = {
  id: 'w1', areaId: 'c1', title: 'Responsive mobile UI', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0, ...NEUTRAL_ACTIVITY,
};

describe('MobileTracks', () => {
  it('has one Area header, a More edit action, and grouped actions above the selected Track', async () => {
    const onEditArea = vi.fn(); const onOpenTrack = vi.fn(); const onBack = vi.fn();
    render(<MobileTracks view="tracks" areas={[area]} tracksByArea={new Map([['c1', [track]]])} areaId="c1" currentTrackId="w1"
      onBack={onBack} onNewTrack={vi.fn()} onOpenSettings={vi.fn()} onCreateArea={vi.fn()} onSelectArea={vi.fn()} onEditArea={onEditArea} onOpenTrack={onOpenTrack}
      readError={null} readLoading={false} onRetryRead={vi.fn()} />);
    const header = screen.getByRole('heading', { name: 'Product' }).closest('header');

    expect(screen.getByRole('button', { name: 'Back to Areas' })).toBeTruthy();
    const more = screen.getByRole('button', { name: 'Area actions' });
    await userEvent.click(more);
    const edit = screen.getByRole('menuitem', { name: 'Edit area Product' });
    expect(more.closest('header')).toBe(header);
    expect(more.querySelector('svg')).not.toBeNull();
    const actions = screen.getByRole('group', { name: 'Workspace actions' });
    expect([...actions.querySelectorAll('button')].map((button) => button.textContent?.trim())).toEqual(['Settings', 'New track']);
    expect(actions.querySelectorAll('ul')).toHaveLength(1);
    expect(actions.querySelectorAll('li svg')).toHaveLength(2);
    expect(header?.nextElementSibling?.contains(actions)).toBe(true);
    await userEvent.click(edit);
    expect(onEditArea).toHaveBeenCalledWith(area);
    const selected = screen.getByRole('button', { name: 'Responsive mobile UI' });
    expect(selected.getAttribute('aria-current')).toBe('page');
    await userEvent.click(selected);
    expect(onOpenTrack).toHaveBeenCalledWith('w1');
    await userEvent.click(screen.getByRole('button', { name: 'Back to Areas' }));
    expect(onBack).toHaveBeenCalledOnce();
  });

  it('never reveals a system Area and still offers onboarding when no user Area is selected', async () => {
    const onCreateArea = vi.fn(); const onNewTrack = vi.fn(); const onEditArea = vi.fn();
    const onBack = vi.fn(); const onOpenSettings = vi.fn();
    render(<MobileTracks view="tracks" areas={[{ ...area, kind: 'system' }]} tracksByArea={new Map([['c1', [track]]])}
      areaId="c1" currentTrackId={undefined} onBack={onBack} onNewTrack={onNewTrack} onOpenSettings={onOpenSettings} onCreateArea={onCreateArea} onSelectArea={vi.fn()}
      onEditArea={onEditArea} onOpenTrack={vi.fn()} readError={null} readLoading={false} onRetryRead={vi.fn()} />);
    expect(screen.queryByRole('button', { name: 'Product' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Responsive mobile UI' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'New area' })).toBeNull();
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'New track' }).disabled).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Area actions' }));
    expect(screen.getByRole('menuitem', { name: 'Edit area' }).getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(screen.getByRole('button', { name: 'New track' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Edit area' }));
    expect(onNewTrack).not.toHaveBeenCalled();
    expect(onEditArea).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Back to Areas' }));
    await userEvent.click(screen.getByRole('button', { name: 'Settings' }));
    expect(onBack).toHaveBeenCalledOnce();
    expect(onOpenSettings).toHaveBeenCalledOnce();
  });

  it('offers only Settings and New area on the Areas index, including an empty workspace', async () => {
    const onCreateArea = vi.fn();
    render(<MobileTracks view="areas" areas={[]} tracksByArea={new Map()} areaId={undefined} currentTrackId={undefined}
      onBack={vi.fn()} onNewTrack={vi.fn()} onOpenSettings={vi.fn()} onCreateArea={onCreateArea} onSelectArea={vi.fn()} onEditArea={vi.fn()} onOpenTrack={vi.fn()}
      readError={null} readLoading={false} onRetryRead={vi.fn()} />);
    const actions = screen.getByRole('group', { name: 'Workspace actions' });
    expect([...actions.querySelectorAll('button')].map((button) => button.textContent?.trim())).toEqual(['Settings', 'New area']);
    expect(screen.queryByRole('button', { name: 'New track' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    expect(onCreateArea).toHaveBeenCalledOnce();
  });

  it('preserves read failure recovery instead of displaying a false empty list', async () => {
    const onRetryRead = vi.fn();
    render(<MobileTracks view="tracks" areas={[area]} tracksByArea={new Map()} areaId="c1" currentTrackId={undefined}
      onBack={vi.fn()} onNewTrack={vi.fn()} onOpenSettings={vi.fn()} onCreateArea={vi.fn()} onSelectArea={vi.fn()} onEditArea={vi.fn()} onOpenTrack={vi.fn()}
      readError="Tracks are unavailable" readLoading={false} onRetryRead={onRetryRead} />);
    expect(screen.getByRole('alert').textContent).toContain('Tracks are unavailable');
    expect(screen.queryByText('No tracks in this area yet.')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryRead).toHaveBeenCalledOnce();
  });
});
