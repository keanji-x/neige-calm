import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { TrackRow } from '../../features/track/row/public.tsx';
import { Sidebar } from './sidebar.tsx';
import { readReceiptsFixture } from '../providers/read-receipts-fixture.tsx';

const receipts = readReceiptsFixture();

afterEach(cleanup);

it('keeps Area actions reachable on a wide no-hover touch display', () => {
  expect(matchMedia('(width >= 60rem)').matches).toBe(true);
  expect(matchMedia('(hover: none)').matches).toBe(true);

  const area: Area = {
    id: 'a1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
  };
  const tracks: Track[] = [null, 10].map((pinnedAt, index) => ({
    id: `t${index}`, areaId: area.id, title: `Touch task ${index}`, sort: index,
    cwd: '/tmp', agentCwd: '/tmp', pinnedAt, closedAt: null, createdAt: 1, updatedAt: 1,
    ...NEUTRAL_ACTIVITY, attention: 'input',
  }));
  render(receipts.wrap(
    <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <Sidebar
        areas={[area]}
        tracksByArea={new Map([['a1', tracks]])}
        tracks={tracks}
        currentPath="/"
        onGo={vi.fn()}
        onRequestCreateArea={vi.fn()}
        onRequestEditArea={vi.fn()}
        onDeleteArea={vi.fn()}
        onNewTrack={vi.fn()}
        onSetPinned={vi.fn()}
        onDeleteTrack={vi.fn()}
        onOpenSettings={vi.fn()}
        onOpenPlugins={vi.fn()}
        onSignOut={vi.fn()}
        collapsed={false}
        onToggleCollapsed={vi.fn()}
      />
    </ThemeProvider>,
  ));

  const actions = document.querySelector<HTMLElement>('[aria-label="Area actions for Work"]');
  expect(actions).not.toBeNull();
  const style = getComputedStyle(actions!.parentElement!);
  expect(style.opacity).toBe('1');
  expect(style.pointerEvents).toBe('auto');
  expect(document.querySelectorAll('[aria-label^="Pin Touch"], [aria-label^="Unpin Touch"]')).toHaveLength(0);
  const menus = document.querySelectorAll<HTMLButtonElement>('button[aria-label^="Actions for track Touch"]');
  expect(menus.length).toBeGreaterThan(0);
  for (const menu of menus) {
    expect(getComputedStyle(menu).opacity).toBe('1');
    expect(getComputedStyle(menu).pointerEvents).toBe('auto');

  }
  for (const row of document.querySelectorAll<HTMLButtonElement>('button[aria-label^="Track Touch"]')) {
    const wrapper = row.parentElement!;
    const status = wrapper.querySelector('[data-nc-activity]')!;
    const menu = wrapper.querySelector<HTMLButtonElement>('button[aria-label^="Actions for track Touch"]')!;
    expect(status).not.toBeNull();
    expect(status.getBoundingClientRect().right).toBeLessThan(menu.getBoundingClientRect().left);
    expect(getComputedStyle(status.parentElement!).opacity).toBe('1');
  }

});

it('keeps a standalone rail pin reachable without covering leading status on touch', async () => {
  const onSetPinned = vi.fn();
  const track: Track = { id: 'standalone', areaId: 'a', title: 'Standalone rail task with a long title that must stop before its pin control', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY, attention: 'input' };
  render(<ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <div style={{ inlineSize: '15rem' }}><TrackRow track={track} variant="rail" presentation={{ kind: 'entry', depth: 0 }} onOpen={vi.fn()} onSetPinned={onSetPinned} /></div>
  </ThemeProvider>);
  const pin = page.getByRole('button', { name: `Pin ${track.title}` });
  const status = document.querySelector('[data-nc-activity="attention"]')!;
  expect(getComputedStyle(pin.element()).opacity).toBe('1');
  expect(status.getBoundingClientRect().right).toBeLessThan(pin.element().getBoundingClientRect().left);
  const row = page.getByRole('button', { name: /^Track Standalone/ }).element();
  expect(row.querySelector('[title]')!.getBoundingClientRect().right).toBeLessThanOrEqual(pin.element().getBoundingClientRect().left);
  await userEvent.click(pin);
  expect(onSetPinned).toHaveBeenCalledWith('standalone', true);
});
