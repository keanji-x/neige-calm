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

async function scene(height: number, longShortcuts = false, initiallyCollapsed = false, noShortcuts = false) {
  await page.viewport(1400, height);
  const preferences = createUiPreferences();
  preferences.setSidebarOrder('sections', ['areas', 'pinned', 'focus']);
  const areas: Area[] = Array.from({ length: 40 }, (_, i) => ({
    id: `a${i}`, name: `Folder ${i}`, color: '#5B8DEF', sort: i, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
  }));
  const tracks: Track[] = Array.from({ length: noShortcuts ? 0 : longShortcuts ? 40 : 2 }, (_, i) => ({
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
  const visible = { top: 0, bottom: window.innerHeight, left: 0, right: window.innerWidth };
  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const css = getComputedStyle(parent);
    const rect = parent.getBoundingClientRect();
    if (/(auto|scroll|hidden|clip)/.test(css.overflowY)) {
      visible.top = Math.max(visible.top, rect.top + parent.clientTop);
      visible.bottom = Math.min(visible.bottom, rect.top + parent.clientTop + parent.clientHeight);
    }
    if (/(auto|scroll|hidden|clip)/.test(css.overflowX)) {
      visible.left = Math.max(visible.left, rect.left + parent.clientLeft);
      visible.right = Math.min(visible.right, rect.left + parent.clientLeft + parent.clientWidth);
    }
  }
  expect(box.top).toBeGreaterThanOrEqual(visible.top - 0.5);
  expect(box.bottom).toBeLessThanOrEqual(visible.bottom + 0.5);
  expect(box.left).toBeGreaterThanOrEqual(visible.left - 0.5);
  expect(box.right).toBeLessThanOrEqual(visible.right + 0.5);
  for (const y of [box.top + 1, box.top + box.height / 2, box.bottom - 1]) {
    expect(element.contains(document.elementFromPoint(box.left + box.width / 2, y)), element.getAttribute('aria-label') ?? '').toBe(true);
  }
}

it('scrolls folders with the wheel while primary navigation, shortcuts and account stay fixed', async () => {
  await scene(900);
  const fixed = ['Today', 'Recipes', 'Collapse Pinned', 'Collapse Focus', 'Open settings'].map(name => button(name).element());
  const before = fixed.map(top);
  const first = button('Collapse area Folder 0').element();
  const folders = first.closest<HTMLElement>('[data-nc-sidebar-scrollport]')!;
  const boundaryTop = top(folders);
  expect(folders.clientTop).toBeGreaterThan(0);
  expect(folders.clientTop).toBeLessThanOrEqual(1);
  const firstTop = top(first);
  expect(top(fixed[2])).toBeLessThan(firstTop);
  expect(top(fixed[3])).toBeLessThan(firstTop);
  await commands.wheelScroll('[aria-label="Collapse area Folder 0"]', 600);
  await expect.poll(() => top(first)).toBeLessThan(firstTop - 100);
  expect(top(folders)).toBe(boundaryTop);
  expect(folders.scrollTop).toBeGreaterThan(100);
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

it.each([200, 250])('reveals complete operation rows through nested scrollports at height %i', async height => {
  await scene(height, true);
  for (const label of ['Pinned', 'Focus']) {
    const group = page.getByRole('group', { name: label, exact: true });
    const more = group.getByRole('button', { name: /^Show \d+ more/ });
    more.element().focus();
    reachable(more.element());
    await more.click();
    for (const row of group.getByRole('button', { name: /^Track Task/ }).elements()) {
      row.focus();
      reachable(row);
      expect(row.getBoundingClientRect().height).toBeGreaterThanOrEqual(button('Today').element().getBoundingClientRect().height);
    }
    const less = group.getByRole('button', { name: `Show less in ${label}` });
    less.element().focus();
    const reveal = less.element();
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
    reachable(reveal);
    await less.click();
    reachable(reveal);
    const row = group.getByRole('button', { name: /^Track Task/ }).first();
    row.element().focus();
    reachable(row.element());
    await row.click();
    await expect.poll(() => document.querySelector('[aria-current="page"]')?.getAttribute('aria-label')).toMatch(/^Track Task/);
    reachable(document.querySelector('[aria-current="page"]')!);
  }
  for (const row of page.getByRole('button', { name: /^Collapse area Folder/ }).elements()) {
    row.focus();
    reachable(row);
  }
  for (const name of ['Collapse area Folder 0', 'New area', 'Today', 'Recipes', 'Open settings']) {
    const target = button(name);
    target.element().focus();
    reachable(target.element());
    await target.click();
  }
  const account = page.getByRole('button', { name: /^Account menu for/ });
  account.element().focus();
  reachable(account.element());
  await account.click();
  await expect.element(page.getByRole('menuitem', { name: 'Settings', exact: true })).toBeVisible();
  await userEvent.keyboard('{Escape}');
});

it.each([200, 250])('keeps folders reachable without shortcuts and after collapse at height %i', async height => {
  const { preferences } = await scene(height, false, false, true);
  const folders = button('New area').element().closest<HTMLElement>('[data-nc-sidebar-scrollport]')!;
  expect(folders.clientTop).toBeGreaterThan(0);
  expect(top(folders)).toBeGreaterThan(button('Recipes').element().getBoundingClientRect().bottom);
  const newArea = button('New area');
  newArea.element().focus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  reachable(newArea.element());
  await newArea.click();
  preferences.setSidebarGroupVisible('areas', false);
  await expect.element(button('New area')).not.toBeInTheDocument();
  expect(folders.isConnected).toBe(false);
  const options = button('Sidebar view options');
  options.element().focus();
  reachable(options.element());
  await options.click();
  await userEvent.keyboard('{Escape}');
  preferences.setSidebarGroupVisible('areas', true);
  await expect.element(button('New area')).toBeInTheDocument();
  const restoredFolders = button('New area').element().closest('[data-nc-sidebar-scrollport]')!;
  await button('Collapse sidebar').click();
  expect(restoredFolders.isConnected).toBe(false);
  const last = button('Show area Folder 39');
  last.element().focus();
  reachable(last.element());
  await userEvent.keyboard('{Enter}');
  const disclosure = button('Collapse area Folder 39');
  await expect.element(disclosure).toHaveFocus();
  reachable(disclosure.element());
  await disclosure.click();
});

it.each([200, 250])('keeps long shortcuts actionable with Areas hidden at height %i', async height => {
  const { preferences } = await scene(height, true);
  preferences.setSidebarGroupVisible('areas', false);
  await expect.element(button('New area')).not.toBeInTheDocument();
  for (const label of ['Pinned', 'Focus']) {
    const disclosure = button(`Collapse ${label}`);
    disclosure.element().focus();
    reachable(disclosure.element());
    await disclosure.click();
    await button(`Expand ${label}`).click();
    const row = page.getByRole('group', { name: label, exact: true }).getByRole('button', { name: /^Track Task/ }).last();
    row.element().focus();
    reachable(row.element());
    await row.click();
  }
  preferences.setSidebarGroupVisible('areas', true);
  await expect.poll(() => document.querySelector('[aria-current="page"]')?.getAttribute('aria-label')).toMatch(/^Track Task/);
  // Restoring Areas reveals its current row even when the route did not change.
  const current = document.querySelector<HTMLButtonElement>('[aria-current="page"]')!;
  reachable(current);
  current.focus();
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  reachable(current);
});


it('uses the Today/Recipes cadence and label column for adjacent collapsed folder headings', async () => {
  const { preferences } = await scene(900);
  for (const label of ['Pinned', 'Focus', 'area Folder 0', 'area Folder 1']) {
    await button(`Collapse ${label}`).click();
  }
  const today = button('Today').element();
  const recipes = button('Recipes').element();
  const stride = top(recipes) - top(today);
  const textLeft = (row: Element) => {
    const range = document.createRange();
    range.selectNodeContents(row.querySelector('[title]')!);
    return range.getBoundingClientRect().left;
  };
  const checkRows = () => {
    const pinned = button('Expand Pinned').element();
    const focus = button('Expand Focus').element();
    const first = button('Expand area Folder 0').element();
    const second = button('Expand area Folder 1').element();
    for (const row of [recipes, pinned, focus, first, second]) {
      reachable(row);
      expect(row.getBoundingClientRect().height).toBe(today.getBoundingClientRect().height);
      expect(textLeft(row)).toBeCloseTo(textLeft(today), 1);
    }
    expect(Math.min(top(pinned), top(focus)) - top(recipes)).toBeCloseTo(stride, 1);
    expect(Math.abs(top(focus) - top(pinned))).toBeCloseTo(stride, 1);
    expect(top(second) - top(first)).toBeCloseTo(stride, 1);
    const folders = first.closest<HTMLElement>('[data-nc-sidebar-scrollport]')!;
    const lastShortcut = top(focus) > top(pinned) ? focus : pinned;
    const above = top(folders) - lastShortcut.getBoundingClientRect().bottom;
    const below = top(first) - top(folders) - folders.clientTop;
    expect(above).toBeGreaterThan(stride - today.getBoundingClientRect().height);
    expect(above).toBeCloseTo(below, 1);
  };
  checkRows();
  // The cadence follows visible siblings, including saved shortcut reordering.
  preferences.setSidebarOrder('sections', ['focus', 'areas', 'pinned']);
  await expect.poll(() => top(button('Expand Focus').element()) - top(button('Expand Pinned').element())).toBeLessThan(0);
  checkRows();
  preferences.setSidebarGroupVisible('focus', false);
  await expect.element(button('Expand Focus')).not.toBeInTheDocument();
  expect(top(button('Expand Pinned').element()) - top(recipes)).toBeCloseTo(stride, 1);
});
