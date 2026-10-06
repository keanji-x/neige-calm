import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';

import '../../styles/entry.css';
import { TrackRow } from '../../features/track/row/public.tsx';
import { renderPage, track } from '../../features/track/page/test-fixtures.tsx';
import { TrackSelector } from './track-selector.tsx';
import { MobilePages } from './mobile-pages.tsx';
import { MobileTracks } from './mobile-tracks.tsx';
import type { Area } from '../../../../core/domain/area.ts';
import { TrackTitle } from '../../features/track/title/public.tsx';

afterEach(cleanup);

const area: Area = {
  id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user',
  defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0,
};

function decoration(host: HTMLElement, title = 'Alpha'): string {
  const text = Array.from(host.querySelectorAll('span')).find((node) => node.textContent === title && node.childElementCount === 0);
  expect(text).toBeTruthy();
  return getComputedStyle(text!).textDecorationLine;
}

const navigation = {
  onBack: () => undefined, onNewTrack: () => undefined,
  onCreateArea: () => undefined, onEditArea: () => undefined, onOpenSettings: () => undefined,
};

describe('shared Track title in production hosts', () => {
  it.each(['default', 'compact', 'panel', 'rail'] as const)('strikes only closed title text in the %s row and restores it on reopen', (variant) => {
    const closed = track({ closedAt: 5, working: true });
    const view = render(<TrackRow track={closed} variant={variant} onOpen={vi.fn()} />);
    const button = screen.getByRole('button', { name: 'Track Alpha, working, closed' });
    expect(decoration(button)).toBe('line-through');
    expect(getComputedStyle(button).textDecorationLine).toBe('none');
    view.rerender(<TrackRow track={track()} variant={variant} onOpen={vi.fn()} />);
    expect(decoration(screen.getByRole('button', { name: 'Track Alpha' }))).toBe('none');
  });

  it('keeps the fallback title decorated for a closed unnamed Track', () => {
    render(<TrackTitle track={track({ title: ' ', closedAt: 0 })} />);
    const text = screen.getByText('Untitled track');
    expect(getComputedStyle(text).textDecorationLine).toBe('line-through');
  });

  it('decorates desktop and mobile page headings and leaves the rename draft plain', async () => {
    await page.viewport(1200, 800);
    renderPage({ track: track({ closedAt: 5 }) });
    const rename = screen.getByRole('button', { name: 'Rename track' });
    expect(decoration(rename)).toBe('line-through');
    expect(decoration(document.querySelector('[data-nc-mobile-header]')!)).toBe('line-through');
    await userEvent.click(rename);
    const input = screen.getByRole('textbox', { name: 'Track title' });
    expect((input as HTMLInputElement).value).toBe('Alpha');
    expect(getComputedStyle(input).textDecorationLine).toBe('none');
    await userEvent.keyboard('{Escape}');
    expect(decoration(screen.getByRole('button', { name: 'Rename track' }))).toBe('line-through');
  });

  it('uses closed titles in the mobile selector and menu while retaining keyboard selection', async () => {
    await page.viewport(414, 896);
    const onSelectTrack = vi.fn();
    const closed = track({ closedAt: 5 });
    render(<TrackSelector track={closed} tracks={[closed, track({ id: 'w2', title: 'Beta' })]}
      loading={false} error={null} onRetry={vi.fn()} onSelectTrack={onSelectTrack}
      controls={{ beginEditing: vi.fn(), titleRef: vi.fn() }} />);
    const trigger = screen.getByRole('button', { name: 'Switch track, Alpha' });
    expect(decoration(trigger)).toBe('line-through');
    await userEvent.click(trigger);
    const item = screen.getByRole('menuitem', { name: 'Alpha, closed' });
    expect(decoration(item)).toBe('line-through');
    await userEvent.keyboard('b{Enter}');
    expect(onSelectTrack).toHaveBeenCalledWith('w2');
  });

  it('uses the same closed title in both mobile navigation lists', async () => {
    await page.viewport(414, 896);
    const closed = track({ closedAt: 5 });
    const onOpenTrack = vi.fn();
    const pages = render(<MobilePages {...navigation} areas={[area]} areaId="c1" tracks={[closed]} onOpenTrack={onOpenTrack} />);
    const title = screen.getByText('Alpha');
    expect(getComputedStyle(title).textDecorationLine).toBe('line-through');
    await userEvent.click(title);
    expect(onOpenTrack).toHaveBeenCalledWith('w1');
    pages.unmount();
    render(<MobileTracks {...navigation} view="tracks" areas={[area]} areaId="c1"
      currentTrackId={undefined} isUnread={() => false} readError={null} readLoading={false} onRetryRead={vi.fn()}
      tracksByArea={new Map([['c1', [closed]]])} onOpenTrack={onOpenTrack} onSelectArea={vi.fn()} />);
    expect(decoration(screen.getByRole('button', { name: 'Alpha, closed' }))).toBe('line-through');
  });
});

/* #2248 review: the rename failure's Dismiss sits beside the input; pressing it must not count as leaving the editor. */
describe('a failed inline rename', () => {
  async function failRename(onRenameTrack: (title: string) => Promise<void>) {
    await page.viewport(1280, 720);
    renderPage({ onRenameTrack });
    await userEvent.click(page.getByRole('button', { name: 'Rename track' }));
    const input = page.getByRole('textbox', { name: 'Track title' });
    await userEvent.clear(input);
    await userEvent.type(input, 'Kept draft');
    await userEvent.keyboard('{Enter}');
    await expect.element(page.getByRole('alert').getByText('The rename is unconfirmed.')).toBeVisible();
    return input;
  }

  it('keeps the draft in the open editor and clears the failure when Dismiss is clicked', async () => {
    const onRenameTrack = vi.fn<(title: string) => Promise<void>>()
      .mockRejectedValueOnce(new Error('socket hang up')).mockResolvedValue(undefined);
    const input = await failRename(onRenameTrack);
    await userEvent.click(page.getByRole('alert').getByRole('button', { name: /^Dismiss/ }));
    expect(screen.queryByRole('alert')).toBeNull();
    await expect.element(input).toHaveValue('Kept draft');
    await expect.element(input).toHaveFocus();
    /* The editor is still the reader's: Enter sends the kept draft again. */
    await userEvent.keyboard('{Enter}');
    expect(onRenameTrack.mock.calls.map(([name]) => name)).toEqual(['Kept draft', 'Kept draft']);
  });

  /* #2256 review: Escape from inside the failure cancels the editor exactly as Escape in the input does, and the
     cancelled edit's failure does not come back when the title is opened again. */
  it('cancels the editor on Escape from Dismiss, as Escape in the input does', async () => {
    await failRename(vi.fn<(title: string) => Promise<void>>().mockRejectedValue(new Error('socket hang up')));
    await userEvent.tab();
    await expect.element(page.getByRole('alert').getByRole('button', { name: /^Dismiss/ })).toHaveFocus();
    await userEvent.keyboard('{Escape}');
    await expect.element(page.getByRole('textbox', { name: 'Track title' })).not.toBeInTheDocument();
    await expect.element(page.getByRole('button', { name: 'Rename track' })).toHaveFocus();
    /* Past the Enter commit's click guard (300 ms), which would swallow an immediate reopen. */
    await new Promise((done) => { setTimeout(done, 350); });
    await userEvent.click(page.getByRole('button', { name: 'Rename track' }));
    await expect.element(page.getByRole('textbox', { name: 'Track title' })).toHaveValue(track().title);
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('keeps the draft when Dismiss is reached and pressed from the keyboard', async () => {
    const input = await failRename(vi.fn<(title: string) => Promise<void>>().mockRejectedValue(new Error('socket hang up')));
    await userEvent.tab();
    await expect.element(page.getByRole('alert').getByRole('button', { name: /^Dismiss/ })).toHaveFocus();
    await userEvent.keyboard('{Enter}');
    expect(screen.queryByRole('alert')).toBeNull();
    await expect.element(input).toHaveValue('Kept draft');
    await expect.element(input).toHaveFocus();
  });
});
