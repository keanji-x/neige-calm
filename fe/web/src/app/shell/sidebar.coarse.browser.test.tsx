import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import type { Area } from '../../../../core/domain/area.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

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
    ...NEUTRAL_ACTIVITY,
  }));
  render(
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
  );

  const actions = document.querySelector<HTMLElement>('[aria-label="Area actions for Work"]');
  expect(actions).not.toBeNull();
  const style = getComputedStyle(actions!.parentElement!);
  expect(style.opacity).toBe('1');
  expect(style.pointerEvents).toBe('auto');
  for (const pin of document.querySelectorAll<HTMLButtonElement>('[aria-label^="Pin Touch"], [aria-label^="Unpin Touch"]')) {
    expect(getComputedStyle(pin).opacity).toBe('1');
    expect(getComputedStyle(pin).pointerEvents).toBe('auto');
  }
  expect(document.querySelectorAll('[aria-label^="Pin Touch"], [aria-label^="Unpin Touch"]')).toHaveLength(3);
  const menus = document.querySelectorAll<HTMLButtonElement>('button[aria-label^="Actions for track Touch"]');
  expect(menus).toHaveLength(3);
  for (const menu of menus) {
    expect(getComputedStyle(menu).opacity).toBe('1');
    expect(getComputedStyle(menu).pointerEvents).toBe('auto');
  }

});
