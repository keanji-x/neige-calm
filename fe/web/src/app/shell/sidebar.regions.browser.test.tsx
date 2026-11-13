import { useState } from '../../ui/state/public.ts';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { createReadReceiptsKernel, readReceiptsFixture } from '../providers/read-receipts-fixture.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { SidebarFixture as Sidebar } from './sidebar-fixture.tsx';
import styles from './shell.module.css';

afterEach(cleanup);

async function scene(height: number, longShortcuts = false, initiallyCollapsed = false) {
  await page.viewport(1400, height);
  const preferences = createUiPreferences();
  preferences.setSidebarOrder('sections', ['areas', 'pinned', 'focus']);
  const areas: Area[] = Array.from({ length: 40 }, (_, i) => ({
    id: `a${i}`, name: `Folder ${i}`, color: '#5B8DEF', sort: i, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
  }));
  const tracks: Track[] = Array.from({ length: longShortcuts ? 40 : 2 }, (_, i) => ({
    id: `t${i}`, title: `Task ${i}`, areaId: 'a39', sort: i, cwd: '/tmp', isolation: 'none', agentCwd: '/tmp',
    pinnedAt: i % 2 ? i : null, closedAt: null, createdAt: 1, updatedAt: i + 1,
    ...NEUTRAL_ACTIVITY, working: true,
  }));
  const { wrap } = readReceiptsFixture(createReadReceiptsKernel());
  function Scene() {
    const [collapsed, setCollapsed] = useState(initiallyCollapsed);
    const [path, setPath] = useState('/');
    return wrap(<UiPreferencesProvider preferences={preferences}>
      <ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
        <div className={`${styles.shell} ${collapsed ? styles.shellCollapsed : styles.shellExpanded}`} style={{ blockSize: '100dvh' }}>
          <Sidebar areas={areas} tracks={tracks} tracksByArea={new Map(areas.map(area => [area.id, tracks.filter(track => track.areaId === area.id)]))}
            currentPath={path} onGo={target => { if (target.name === 'track') setPath(`/track/${target.trackId}`); }}
            collapsed={collapsed} onToggleCollapsed={() => setCollapsed(!collapsed)}
            onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()} onDeleteArea={vi.fn()} onNewTrack={vi.fn()}
            onSetPinned={vi.fn()} onDeleteTrack={vi.fn()} onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()} />
          <main />
        </div>
      </ThemeProvider>
    </UiPreferencesProvider>);
  }
  const view = render(<Scene />);
  return { preferences, view };
}

const button = (name: string) => page.getByRole('button', { name, exact: true });
const top = (element: Element) => element.getBoundingClientRect().top;
function reachable(element: Element) {
  const box = element.getBoundingClientRect();
  expect(box.height).toBeGreaterThan(0);
  expect(box.top).toBeGreaterThanOrEqual(0);
  expect(box.bottom).toBeLessThanOrEqual(window.innerHeight);
  expect(element.contains(document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2))).toBe(true);
}

it('scrolls folders with the wheel while primary navigation, shortcuts and account stay fixed', async () => {
  await scene(900);
  const fixed = ['Today', 'Recipes', 'Collapse Pinned', 'Collapse Focus', 'Open settings'].map(name => button(name).element());
  const before = fixed.map(top);
  const first = button('Collapse area Folder 0').element();
  const firstTop = top(first);
  expect(top(fixed[2])).toBeLessThan(firstTop);
  expect(top(fixed[3])).toBeLessThan(firstTop);
  await commands.wheelScroll('[aria-label="Collapse area Folder 0"]', 600);
  await expect.poll(() => top(first)).toBeLessThan(firstTop - 100);
  expect(fixed.map(top)).toEqual(before);
  fixed.forEach(reachable);
  for (const name of ['Pinned', 'Focus']) {
    page.getByRole('group', { name, exact: true }).getByRole('button', { name: /^Track Task/ }).elements().forEach(reachable);
  }
  const newArea = button('New area').element();
  newArea.focus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  await expect.element(button('New area')).toHaveFocus();
  reachable(newArea);
  expect(fixed.map(top)).toEqual(before);
});

it.each([900, 400])('keeps long shortcuts and folders independently reachable at height %i', async height => {
  await scene(height, true);
  // Saved Areas-first ordering must not move the folder region above shortcuts.
  const first = button('Collapse area Folder 0').element();
  expect(top(button('Collapse Pinned').element())).toBeLessThan(top(first));
  await page.getByRole('button', { name: /^Show \d+ more in Pinned$/ }).click();
  const pinnedFirst = page.getByRole('group', { name: 'Pinned', exact: true }).getByRole('button', { name: /^Track Task/ }).first().element();
  const shortcutTop = top(pinnedFirst);
  const folderTop = top(first);
  await commands.wheelScroll('[aria-label="Pinned"]', 180, { x: 30, y: 45 });
  await expect.poll(() => top(pinnedFirst)).toBeLessThan(shortcutTop);
  expect(top(first)).toBe(folderTop);
  const focus = button('Collapse Focus').element();
  focus.focus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  reachable(focus);
  await page.getByRole('button', { name: /^Show \d+ more in Focus$/ }).click();
  for (const label of ['Pinned', 'Focus']) {
    const group = page.getByRole('group', { name: label, exact: true });
    const rows = group.getByRole('button', { name: /^Track Task/ }).elements();
    expect(rows).toHaveLength(20);
    for (const row of rows) { row.focus(); reachable(row); }
    const less = group.getByRole('button', { name: `Show less in ${label}` });
    const revealButton = less.element();
    await less.click();
    reachable(revealButton);
  }
  first.focus();
  reachable(first);
  const fixed = ['Today', 'Recipes', 'Open settings'].map(name => button(name).element());
  fixed.forEach(reachable);
  // Navigating from a shortcut reveals the canonical current row only in the folder scrollport.
  await page.getByRole('group', { name: 'Pinned', exact: true }).getByRole('button', { name: /^Track Task/ }).first().click();
  const current = document.querySelector('[aria-current="page"]')!;
  reachable(current);
  fixed.forEach(reachable);
  expect(document.scrollingElement!.scrollTop).toBe(0);
});

it('scrolls the collapsed folder strip and restores the expanded disclosure focus', async () => {
  await scene(400, false, true);
  const expand = button('Expand sidebar').element();
  const account = page.getByRole('button', { name: /^Account menu for/ }).element();
  const before = [top(expand), top(account)];
  const first = button('Show area Folder 0').element();
  const firstTop = top(first);
  await commands.wheelScroll('[aria-label="Show area Folder 0"]', 600);
  await expect.poll(() => top(first)).toBeLessThan(firstTop - 100);
  expect([top(expand), top(account)]).toEqual(before);
  const last = button('Show area Folder 39');
  last.element().focus();
  reachable(last.element());
  await userEvent.keyboard('{Enter}');
  await expect.element(button('Collapse area Folder 39')).toHaveFocus();
  reachable(button('Collapse area Folder 39').element());
  reachable(button('Today').element());
  reachable(button('Open settings').element());
});

it('confines current-row reveal to the folder scrollport inside an outer scrolling host', async () => {
  const { view } = await scene(400);
  const host = view.container;
  host.style.blockSize = '250px';
  host.style.overflow = 'auto';
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  // Direct activation avoids the driver's own automatic scroll-to-click behavior.
  fireEvent.click(page.getByRole('group', { name: 'Pinned', exact: true })
    .getByRole('button', { name: /^Track Task/ }).first().element());
  await expect.poll(() => document.querySelector('[aria-current="page"]')?.getAttribute('aria-label')).toMatch(/^Track Task/);
  const current = document.querySelector('[aria-current="page"]')!;
  expect(top(current)).toBeGreaterThan(top(rail));
  expect(current.getBoundingClientRect().bottom).toBeLessThan(button('Open settings').element().getBoundingClientRect().top);
  expect(host.scrollTop).toBe(0);
  expect(document.scrollingElement!.scrollTop).toBe(0);
});
