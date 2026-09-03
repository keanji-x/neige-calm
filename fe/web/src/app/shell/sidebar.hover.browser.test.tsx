import { cleanup, render, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(cleanup);

it('reveals Area delete only while the row is hovered on a desktop pointer', async () => {
  await page.viewport(1400, 900);
  expect(matchMedia('(width >= 60rem)').matches).toBe(true);
  expect(matchMedia('(hover: hover)').matches).toBe(true);

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

  const remove = document.querySelector<HTMLElement>('[aria-label="Delete area Work"]')!;
  await page.getByRole('button', { name: 'New area' }).hover();
  expect(getComputedStyle(remove).opacity).toBe('0');
  expect(getComputedStyle(remove).pointerEvents).toBe('none');

  await page.getByRole('button', { name: 'Collapse area Work' }).hover();
  await waitFor(() => expect(getComputedStyle(remove).opacity).toBe('1'));
  expect(getComputedStyle(remove).pointerEvents).toBe('auto');

  await page.getByRole('button', { name: 'Delete area Work' }).click();
  await expect.element(page.getByRole('dialog', { name: 'Delete Work?' })).toBeVisible();
});
