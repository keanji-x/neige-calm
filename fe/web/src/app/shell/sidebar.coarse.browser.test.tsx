import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';

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
    createdAt: 1, updatedAt: 1,
  };
  render(
    <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <Sidebar
        areas={[area]}
        tracksByArea={new Map([['a1', []]])}
        tracks={[]}
        currentPath="/"
        onGo={vi.fn()}
        onCreateArea={vi.fn()}
        onRenameArea={vi.fn()}
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

  const trigger = document.querySelector<HTMLElement>('[aria-label="Area actions for Work"]');
  expect(trigger).not.toBeNull();
  const wrapper = trigger!.parentElement;
  expect(wrapper).not.toBeNull();
  const style = getComputedStyle(wrapper!);
  expect(style.opacity).toBe('1');
  expect(style.pointerEvents).toBe('auto');
});
