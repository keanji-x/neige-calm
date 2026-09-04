import { cleanup, render, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

type Rgb = readonly [number, number, number];

function paintedRgb(color: string): Rgb {
  const canvas = document.createElement('canvas');
  canvas.width = 1;
  canvas.height = 1;
  const context = canvas.getContext('2d')!;
  context.fillStyle = color;
  context.fillRect(0, 0, 1, 1);
  const [red = 0, green = 0, blue = 0] = context.getImageData(0, 0, 1, 1).data;
  return [red, green, blue];
}

function contrastRatio(foreground: string, background: string): number {
  const luminance = (rgb: Rgb) => rgb.map((channel) => channel / 255)
    .map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4)
    .reduce((sum, channel, index) => sum + channel * [0.2126, 0.7152, 0.0722][index], 0);
  const values = [luminance(paintedRgb(foreground)), luminance(paintedRgb(background))]
    .toSorted((left, right) => right - left);
  return ((values[0] ?? 0) + 0.05) / ((values[1] ?? 0) + 0.05);
}

it('keeps Area actions visible without requiring hover on a desktop pointer', async () => {
  await page.viewport(1400, 900);
  expect(matchMedia('(width >= 60rem)').matches).toBe(true);
  expect(matchMedia('(hover: hover)').matches).toBe(true);

  const area: Area = {
    id: 'a1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
  };
  render(
    <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <Sidebar
        areas={[area]}
        tracksByArea={new Map([['a1', []]])}
        tracks={[]}
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

  const actions = document.querySelector<HTMLElement>('[aria-label="Area actions for Work"]')!;
  const actionsWrap = actions.parentElement!;
  await page.getByRole('button', { name: 'New area' }).hover();
  expect(getComputedStyle(actionsWrap).opacity).toBe('1');
  expect(getComputedStyle(actionsWrap).pointerEvents).toBe('auto');

  await page.getByRole('button', { name: 'Collapse area Work' }).hover();
  expect(getComputedStyle(actionsWrap).opacity).toBe('1');
  expect(getComputedStyle(actionsWrap).pointerEvents).toBe('auto');

  await page.getByRole('button', { name: 'Area actions for Work' }).click();
  await page.getByRole('menuitem', { name: 'Delete area' }).click();
  await expect.element(page.getByRole('dialog', { name: 'Delete Work?' })).toBeVisible();
});

it('keeps the clickable Area quieter and uses the same cadence as its Track rows', async () => {
  await page.viewport(1400, 900);
  const area: Area = {
    id: 'a1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
  };
  const emptyArea: Area = {
    ...area, id: 'a0', name: 'Empty', sort: 0,
  };
  const nextArea: Area = {
    ...area, id: 'a2', name: 'Next', sort: 2,
  };
  const tracks: Track[] = ['First', 'Second'].map((title, index) => ({
    id: `t${index}`, areaId: area.id, title, sort: index, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 1, updatedAt: 1,
    ...NEUTRAL_ACTIVITY,
  }));
  render(
    <ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
      <Sidebar
        areas={[emptyArea, area, nextArea]}
        tracksByArea={new Map([[emptyArea.id, []], [area.id, tracks], [nextArea.id, []]])}
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

  const disclosure = document.querySelector<HTMLElement>('[aria-label="Collapse area Work"]')!;
  const areaRow = disclosure;
  const areaName = document.querySelector<HTMLElement>('[title="Work"]')!;
  const first = document.querySelector<HTMLElement>('[aria-label^="Track First"]')!;
  const second = document.querySelector<HTMLElement>('[aria-label^="Track Second"]')!;
  const nextDisclosure = document.querySelector<HTMLElement>('[aria-label="Collapse area Next"]')!;
  const areaTopBeforeEmptyCollapse = areaRow.getBoundingClientRect().top;
  await page.getByRole('button', { name: 'Collapse area Empty' }).click();
  await expect.element(page.getByRole('button', { name: 'Expand area Empty' })).toBeVisible();
  const areaRect = areaRow.getBoundingClientRect();
  const firstRect = first.getBoundingClientRect();
  const secondRect = second.getBoundingClientRect();
  const nextRect = nextDisclosure.getBoundingClientRect();
  const restBackground = getComputedStyle(areaRow).backgroundColor;

  expect(areaName.closest('button')).toBe(areaRow);
  expect(areaRect.top).toBeCloseTo(areaTopBeforeEmptyCollapse, 3);
  expect(areaRect.height).toBe(firstRect.height);
  expect(firstRect.top - areaRect.top).toBeCloseTo(secondRect.top - firstRect.top, 3);
  expect(nextRect.top - secondRect.top).toBeCloseTo(secondRect.top - firstRect.top, 3);
  for (const theme of ['light', 'dark']) {
    document.documentElement.dataset.theme = theme;
    const railBackground = getComputedStyle(areaRow.closest('nav')!).backgroundColor;
    const areaContrast = contrastRatio(getComputedStyle(areaName).color, railBackground);
    const canonicalMutedContrast = contrastRatio(
      getComputedStyle(areaRow.querySelector<HTMLElement>('[aria-hidden="true"]')!).color,
      railBackground,
    );
    expect(areaContrast).toBeGreaterThanOrEqual(4.5);
    expect(areaContrast).toBeLessThan(canonicalMutedContrast);
  }
  document.documentElement.dataset.theme = 'light';
  const restColor = getComputedStyle(areaName).color;
  await page.getByText('Work', { exact: true }).hover();
  await waitFor(() => expect(getComputedStyle(areaName).color).not.toBe(restColor));
  expect(getComputedStyle(areaRow).backgroundColor).not.toBe(restBackground);
});
