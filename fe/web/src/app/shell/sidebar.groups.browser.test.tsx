import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { createReadReceiptsKernel, readReceiptsFixture } from '../providers/read-receipts-fixture.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import styles from './shell.module.css';
import { Sidebar } from './sidebar.tsx';
import { TrackRow } from '../../features/track/row/public.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

async function scene(activitiesVisible: boolean, areaName = 'Work') {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  /* The kernel's baseline is 1, so a Track whose `activityAt` is later reads as unread. */
  const kernel = createReadReceiptsKernel({ baselineMs: 1 });
  const { wrap } = readReceiptsFixture(kernel);
  if (activitiesVisible) {
    preferences.setSidebarGroupVisible('unread', true);
    preferences.setSidebarGroupVisible('running', true);
  }
  const area: Area = { id: 'work', name: areaName, color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const reading: Area = { ...area, id: 'reading', name: 'Reading', sort: 2 };
  const tracks: Track[] = [
    { id: 'review', title: 'Review result', attention: 'input' as const, working: false },
    { id: 'running', title: 'Build frontend', attention: 'none' as const, working: true },
    { id: 'loose', title: 'Next task', attention: 'none' as const, working: false, pinnedAt: null, activityAt: null },
  ].map((row) => ({ areaId: area.id, sort: 1, cwd: '/tmp', agentCwd: '/tmp', pinnedAt: 10,
    closedAt: null, createdAt: 1, updatedAt: 10, ...NEUTRAL_ACTIVITY, activityAt: 10, ...row }));
  const onGo = vi.fn();
  const onSetPinned = vi.fn();
  const sidebar = (tracks: Track[]) => wrap(<UiPreferencesProvider preferences={preferences}>
    <ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
      <div className={`${styles.shell} ${styles.shellExpanded}`} style={{ blockSize: '100dvh' }}>
        <Sidebar areas={[area, reading]} tracksByArea={new Map([[area.id, tracks], [reading.id, []]])} tracks={tracks}
          currentPath="/track/running" onGo={onGo} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
          onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={onSetPinned} onDeleteTrack={vi.fn()}
          onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()}
          collapsed={false} onToggleCollapsed={vi.fn()} />
        <main />
      </div>
    </ThemeProvider>
  </UiPreferencesProvider>);
  const view = render(sidebar(tracks));
  await expect.poll(() => kernel.requests.length).toBe(1);

  const options = page.getByRole('button', { name: 'Sidebar view options' });
  const collapse = page.getByRole('button', { name: 'Collapse sidebar' });
  const today = page.getByRole('button', { name: 'Go to Today' });
  const areasDisclosure = page.getByRole('button', { name: 'Collapse Areas', exact: true });
  return { kernel, preferences, tracks, sidebar, view, onGo, onSetPinned, options, collapse, today, areasDisclosure };
}

it('keeps category headers glyph-free and reserves the disclosure glyph for Areas', async () => {
  await scene(true);
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  for (const width of ['15rem', '11rem']) {
    rail.style.inlineSize = width;
    const area = page.getByRole('button', { name: 'Collapse area Work', exact: true }).element();
    const section = page.getByRole('button', { name: 'Collapse Areas', exact: true }).element();
    expect(area.querySelectorAll('svg').length).toBe(1);
    expect(section.querySelectorAll('svg').length).toBe(0);
    const leading = area.querySelector('svg')!.parentElement!;
    const captionStart = leading.getBoundingClientRect().left;
    expect(textBounds(area.querySelector('[title]')!).left).toBeGreaterThan(captionStart);
    for (const title of ['Running', 'Unread', 'Pinned', 'Waiting on you', 'Areas']) {
      for (const action of ['Collapse', 'Expand']) {
        const disclosure = page.getByRole('button', { name: `${action} ${title}`, exact: true });
        expect(textBounds(disclosure.element().querySelector('[title]')!).left).toBeCloseTo(captionStart, 1);
        await disclosure.hover();
        expect(disclosure.element().querySelectorAll('svg').length).toBe(0);
        await disclosure.click();
      }
    }
  }
});

it('aligns Area tracks with the Area label and fills the selected row', async () => {
  await scene(true);
  const group = page.getByRole('group', { name: 'area Work', exact: true });
  const areaTitle = group.getByRole('button', { name: 'Collapse area Work', exact: true }).element().querySelector('[title]')!;
  const selected = group.getByRole('button', { name: /^Track Build frontend/ }).element();
  expect(textBounds(selected.querySelector('[title]')!).left).toBeCloseTo(textBounds(areaTitle).left, 1);
  expect(parseFloat(getComputedStyle(selected, '::before').insetInlineStart)).toBe(0);
  expect(parseFloat(getComputedStyle(selected, '::before').insetInlineStart)).toBeCloseTo(
    textBounds(selected.querySelector('[title]')!).left - textBounds(areaTitle).left, 1);
  expect(selected.getAttribute('aria-current')).toBe('page');
});

it.each(['light', 'dark'])('uses the same rail background bounds for hover and selection in %s', async (theme) => {
  await scene(true);
  document.documentElement.dataset.theme = theme;
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  const group = page.getByRole('group', { name: 'area Work', exact: true });
  const selected = group.getByRole('button', { name: /^Track Build frontend/ });
  const idle = group.getByRole('button', { name: /^Track Next task/ });
  for (const width of ['15rem', '11rem']) {
    rail.style.inlineSize = width;
    const bounds = (element: Element) => {
      const paint = getComputedStyle(element, '::before');
      return [paint.insetInlineStart, paint.insetInlineEnd, paint.width, paint.height, paint.borderRadius];
    };
    await page.getByRole('button', { name: 'Go to Today' }).hover();
    const expected = bounds(selected.element());
    expect(parseFloat(expected[0])).toBe(0);
    for (const row of [idle, selected]) {
      await row.hover();
      expect(bounds(row.element())).toEqual(expected);
      expect(getComputedStyle(row.element()).backgroundColor).toBe('rgba(0, 0, 0, 0)');
      expect(getComputedStyle(row.element(), '::before').backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
    }
    const shortcut = page.getByRole('group', { name: 'Pinned', exact: true }).getByRole('button', { name: /^Track Build frontend/ });
    await shortcut.hover();
    expect(getComputedStyle(shortcut.element(), '::before').insetInlineStart).toBe('0px');
    expect(getComputedStyle(shortcut.element()).backgroundColor).toBe('rgba(0, 0, 0, 0)');
  }
});

it('keeps activity markers before their own Track text at every hierarchy level', async () => {
  const { today } = await scene(true);
  await today.hover();
  const markers = document.querySelectorAll('[data-nc-activity]');
  expect(markers.length).toBeGreaterThan(0);
  const first = markers[0].getBoundingClientRect();
  const globalCenter = first.left + first.width / 2;
  for (const marker of markers) {
    const box = marker.getBoundingClientRect();
    expect(box.left + box.width / 2).toBeCloseTo(globalCenter, 1);
    const row = marker.parentElement!.parentElement!;
    const title = row.querySelector('[title]')!;
    expect(marker.getBoundingClientRect().right).toBeLessThan(title.getBoundingClientRect().left);
  }
});

it.each(['rail', 'panel'] as const)('centres both activity sizes in the %s action slot', (variant) => {
  const track: Track = { id: 'slot', title: 'Slot geometry', areaId: 'a', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY };
  const tree = (working: boolean) => <ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
    <div style={{ inlineSize: '15rem' }}><TrackRow track={{ ...track, working }} {...(variant === 'rail' ? { variant, presentation: { kind: 'entry' as const, depth: 0 as const } } : { variant })}
      unread={!working} onOpen={vi.fn()} onDelete={vi.fn()} onSetPinned={vi.fn()} /></div>
  </ThemeProvider>;
  const view = render(tree(false));
  const centerX = (element: Element) => {
    const box = element.getBoundingClientRect();
    return box.left + box.width / 2;
  };
  for (const working of [false, true]) {
    view.rerender(tree(working));
    const marker = document.querySelector('[data-nc-activity]')!;
    const control = page.getByRole('button', { name: variant === 'rail' ? 'Pin Slot geometry' : 'Delete Slot geometry', exact: true }).element();
    if (variant === 'rail') expect(centerX(marker)).toBeLessThan(centerX(control));
    else expect(centerX(marker)).toBeCloseTo(centerX(control), 1);
  }
});

it.each(['light', 'dark'] as const)('preserves label alignment and leading status when sizing tokens change in %s mode', async (theme) => {
  await scene(true);
  document.documentElement.dataset.theme = theme;
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  rail.style.setProperty('--control-h-sm', '24px');
  rail.style.setProperty('--space-2', '6px');
  const group = page.getByRole('group', { name: 'area Work', exact: true });
  const area = group.getByRole('button', { name: 'Collapse area Work', exact: true }).element();
  for (const row of group.getByRole('button', { name: /^Track / }).elements()) {
    expect(row.querySelector('[title]')!.getBoundingClientRect().left)
      .toBeCloseTo(area.querySelector('[title]')!.getBoundingClientRect().left, 1);
  }
  for (const marker of document.querySelectorAll('[data-nc-activity]')) {
    const title = marker.parentElement!.parentElement!.querySelector('[title]')!;
    expect(marker.getBoundingClientRect().right).toBeLessThan(title.getBoundingClientRect().left);
  }
  expect(rail.scrollWidth).toBe(rail.clientWidth);
});

it('keeps long rail labels within the shared columns without horizontal scrolling', async () => {
  const areaName = 'A long workspace area 用于验证窄侧边栏的对齐与截断';
  const title = 'A long task title 用于验证文字与右侧操作之间的边界';
  const { view, sidebar, tracks, today } = await scene(true, areaName);
  view.rerender(sidebar(tracks.map(track => ({ ...track, title }))));
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  for (const width of ['11rem', '18rem']) {
    rail.style.inlineSize = width;
    await today.hover();
    const expected = today.element().lastElementChild!.getBoundingClientRect().left;
    const areaButton = page.getByRole('button', { name: `Collapse area ${areaName}`, exact: true });
    const areaTitle = areaButton.element().querySelector<HTMLElement>('[title]')!;
    expect(areaTitle.getBoundingClientRect().left).toBe(expected);
    const areaMenu = page.getByRole('button', { name: `Area actions for ${areaName}`, exact: true });
    expect(areaTitle.getBoundingClientRect().right).toBeLessThanOrEqual(areaMenu.element().getBoundingClientRect().left);
    const group = page.getByRole('group', { name: `area ${areaName}`, exact: true });
    const row = group.getByRole('button', { name: /^Track / });
    await row.first().hover();
    const caption = row.first().element().querySelector<HTMLElement>('[title]')!;
    expect(caption.getBoundingClientRect().left).toBeCloseTo(areaTitle.getBoundingClientRect().left, 1);
    const menu = group.getByRole('button', { name: `Actions for track ${title}`, exact: true }).first();
    expect(caption.getBoundingClientRect().right).toBeLessThanOrEqual(menu.element().getBoundingClientRect().left);
    expect(rail.scrollWidth).toBe(rail.clientWidth);
    expect(rail.scrollLeft).toBe(0);
  }
});

it('lets clicks in a decorative status slot reach Track navigation', async () => {
  await page.viewport(1400, 900);
  const onOpen = vi.fn();
  const track: Track = { id: 'slot', title: 'Status target', areaId: 'a', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY, working: true };
  render(<ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
    <div style={{ inlineSize: '15rem' }}><TrackRow track={track} variant="rail" presentation={{ kind: 'entry', depth: 0 }} onOpen={onOpen} /></div>
  </ThemeProvider>);
  const row = page.getByRole('button', { name: /^Track Status target/ });
  const box = row.element().getBoundingClientRect();
  await row.click({ position: { x: box.width - 14, y: box.height / 2 } });
  expect(onOpen).toHaveBeenCalledExactlyOnceWith('slot');
});

it('places view options and preserves disclosure and keyboard navigation', async () => {
  const { onGo, options, collapse, today, areasDisclosure } = await scene(false);
  const areasTitle = page.getByRole('button', { name: 'Collapse Areas', exact: true }).element()
    .querySelector<HTMLElement>('[title="Areas"]')!;
  const areaTitle = page.getByRole('button', { name: 'Collapse area Work', exact: true }).element()
    .querySelector<HTMLElement>('[title="Work"]')!;
  expect(textBounds(areaTitle).left).toBeGreaterThan(textBounds(areasTitle).left);

  const areasButton = areasDisclosure.element();
  expect(areasButton.querySelector('svg')).toBeNull();
  expect(areasButton.getAttribute('aria-expanded')).toBe('true');
  await areasDisclosure.click();
  expect(areasButton.getAttribute('aria-expanded')).toBe('false');
  await page.getByRole('button', { name: 'Expand Areas', exact: true }).click();
  expect(areasButton.getAttribute('aria-expanded')).toBe('true');
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  await expect.element(areasDisclosure).toHaveFocus();
  await today.click();
  expect(onGo).toHaveBeenCalledExactlyOnceWith({ name: 'today' });
  onGo.mockClear();

  const optionsBox = options.element().getBoundingClientRect();
  const collapseBox = collapse.element().getBoundingClientRect();
  expect(today.element().getBoundingClientRect().right).toBeLessThanOrEqual(optionsBox.left);
  expect(optionsBox.right).toBeLessThanOrEqual(collapseBox.left);
  expect(optionsBox.top + optionsBox.height / 2).toBeCloseTo(collapseBox.top + collapseBox.height / 2, 0);
  const newAreaBox = page.getByRole('button', { name: 'New area', exact: true }).element().getBoundingClientRect();
  expect(newAreaBox.width).toBeCloseTo(28, 0);
  expect(newAreaBox.height).toBeCloseTo(28, 0);
  const centerX = (element: Element) => {
    const box = element.getBoundingClientRect();
    return box.left + box.width / 2;
  };
  const areaMenu = page.getByRole('button', { name: 'Area actions for Work', exact: true }).element();
  const areaPlus = page.getByRole('button', { name: 'New track in Work', exact: true }).element();
  const workGroup = page.getByRole('group', { name: 'area Work', exact: true });
  const trackMenu = workGroup.getByRole('button', { name: 'Actions for track Review result', exact: true }).element();
  const trackDelete = workGroup.getByRole('button', { name: 'Delete Review result', exact: true }).element();
  expect(centerX(trackMenu)).toBeLessThan(centerX(trackDelete));
  expect(centerX(trackMenu)).toBeCloseTo(centerX(areaMenu), 0);
  expect(centerX(trackDelete)).toBeCloseTo(centerX(areaPlus), 0);
  expect(centerX(options.element())).toBeCloseTo(centerX(areaMenu), 0);
  expect(centerX(page.getByRole('button', { name: 'Group actions for Pinned', exact: true }).element())).toBeCloseTo(centerX(areaMenu), 0);
  expect(centerX(page.getByRole('button', { name: 'Group actions for Areas', exact: true }).element())).toBeCloseTo(centerX(areaMenu), 0);
  expect(centerX(collapse.element())).toBeCloseTo(centerX(areaPlus), 0);

  await options.click();
  await expect.element(page.getByRole('menuitem', { name: 'Hidden groups' })).toBeVisible();
  await userEvent.keyboard('{ArrowDown}');
  await expect.element(page.getByRole('menuitem', { name: 'Hidden groups' })).toHaveFocus();
  await userEvent.keyboard('{ArrowRight}');
  await expect.element(page.getByRole('menuitem', { name: 'Show Unread' })).toHaveFocus();
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('menu')).not.toBeInTheDocument();
  await expect.element(options).toHaveFocus();
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('menuitem', { name: 'Hidden groups' })).toHaveFocus();
  await userEvent.keyboard('{ArrowRight}');
  await expect.element(page.getByRole('menuitem', { name: 'Show Running' })).toHaveFocus();
  await userEvent.keyboard(' ');
  await expect.element(page.getByRole('menu')).not.toBeInTheDocument();
  await expect.element(options).toHaveFocus();


});

it('manages group ordering, visibility and pointer restoration', async () => {
  const { options, onGo } = await scene(true);
  for (const title of ['Waiting on you', 'Pinned', 'Unread', 'Running', 'Areas']) {
    const button = page.getByRole('button', { name: `Collapse ${title}`, exact: true });
    expect(button.element().getBoundingClientRect().height).toBeCloseTo(28, 0);
    await button.click();
    await expect.element(page.getByRole('button', { name: `Expand ${title}`, exact: true })).toHaveAttribute('aria-expanded', 'false');
    await page.getByRole('button', { name: `Expand ${title}`, exact: true }).click();
  }
  await page.getByRole('button', { name: 'Group actions for Pinned' }).click();
  await page.getByRole('menuitem', { name: 'Move up' }).click();
  expect(document.querySelector('nav h2')?.textContent).toBe('Pinned');
  await expect.element(page.getByRole('button', { name: 'Group actions for Pinned' })).toHaveFocus();
  await page.getByRole('button', { name: 'Group actions for Pinned' }).click();
  await page.getByRole('menuitem', { name: 'Hide group' }).click();
  await expect.element(options).toHaveFocus();
  await expect.element(page.getByRole('group', { name: 'Pinned', exact: true })).not.toBeInTheDocument();
  await options.click();
  await page.getByRole('menuitem', { name: 'Hidden groups' }).click();
  await page.getByRole('menuitem', { name: 'Show Pinned' }).click();
  expect(document.querySelector('nav h2')?.textContent).toBe('Pinned');

  await page.getByRole('button', { name: 'Area actions for Work' }).click();
  await page.getByRole('menuitem', { name: 'Move down' }).click();
  expect(page.getByRole('button', { name: /^Collapse area / }).elements().map(node => node.textContent)).toEqual(['Reading', 'Work']);
  await page.getByRole('button', { name: 'Area actions for Work' }).click();
  await page.getByRole('menuitem', { name: 'Hide group' }).click();
  await expect.element(options).toHaveFocus();
  await options.click();
  await page.getByRole('menuitem', { name: 'Hidden groups' }).click();
  await page.getByRole('menuitem', { name: 'Show area Work' }).click();
  expect(page.getByRole('button', { name: /^Collapse area / }).elements().map(node => node.textContent)).toEqual(['Reading', 'Work']);
  expect(onGo).not.toHaveBeenCalled();
  await options.click();
  await expect.element(page.getByRole('menuitem', { name: 'Hidden groups' })).toHaveAttribute('aria-disabled', 'true');
  await userEvent.keyboard('{Escape}');

});

it('keeps pin controls usable and updates live group membership', async () => {
  const { today, onGo, onSetPinned, kernel, tracks, sidebar, view } = await scene(true);
  await today.hover();
  expect(page.getByRole('button', { name: 'Pin Next task' }).query()).toBeNull();
  const runningGroup = page.getByRole('group', { name: 'Running', exact: true });
  const runningRow = runningGroup.getByRole('button', { name: /^Track Build frontend/ });
  const menu = runningGroup.getByRole('button', { name: 'Actions for track Build frontend' });
  await runningRow.hover();
  await menu.click();
  await page.getByRole('menuitem', { name: 'Unpin globally', exact: true }).click();
  expect(onSetPinned).toHaveBeenCalledWith('running', false);
  await page.getByRole('group', { name: 'Running', exact: true }).getByRole('button', { name: /^Track Build frontend/ }).click();
  expect(onGo).toHaveBeenCalledWith({ name: 'track', trackId: 'running' });
  expect(document.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
  // Another device reads both; this one sees it when its tab is shown again.
  for (const id of ['review', 'running']) {
    await kernel.transport.send({ method: 'POST', path: `/api/read-receipts/track/${id}/read`, credentials: 'include', body: { readThroughMs: 10 } });
  }
  window.dispatchEvent(new Event('visibilitychange'));
  await expect.element(page.getByRole('group', { name: 'Unread', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Unread', exact: true }).getByRole('button', { name: /^Track / })).not.toBeInTheDocument();
  await expect.element(page.getByRole('group', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Running', exact: true })).toBeVisible();
  view.rerender(sidebar(tracks.map((track) => ({ ...track, working: false }))));
  await expect.element(page.getByRole('group', { name: 'Running', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Running', exact: true }).getByRole('button', { name: /^Track / })).not.toBeInTheDocument();
});

it('recovers hidden groups through the full pointer list and Escape layers', async () => {
  const { options } = await scene(true);
  // Pointer recovery remains usable even after inspecting and closing the full hidden list.
  const recoverable = ['Pinned', 'Waiting on you', 'Unread', 'Running', 'Areas'];
  for (const title of recoverable) {
    await page.getByRole('button', { name: `Group actions for ${title}` }).click();
    await page.getByRole('menuitem', { name: 'Hide group', exact: true }).click();
  }
  await options.click();
  await page.getByRole('menuitem', { name: 'Hidden groups' }).click();
  for (const title of recoverable) await expect.element(page.getByRole('menuitem', { name: `Show ${title}`, exact: true })).toBeVisible();
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('menuitem', { name: 'Hidden groups' })).toHaveAttribute('aria-expanded', 'false');
  await userEvent.keyboard('{Escape}');
  await expect.element(options).toHaveAttribute('aria-expanded', 'false');
  for (const title of recoverable) {
    await options.click();
    const hidden = page.getByRole('menuitem', { name: 'Hidden groups' });
    // Hover opens this submenu; a later click intentionally toggles it closed after the vendor guard.
    await hidden.hover();
    await expect.element(hidden).toHaveAttribute('aria-expanded', 'true');
    await page.getByRole('menuitem', { name: `Show ${title}`, exact: true }).click();
    await expect.element(page.getByRole('group', { name: title, exact: true })).toBeVisible();
  }

});


it('uses the three-dot Track menu with keyboard and keeps actions separate from navigation', async () => {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  preferences.setDatabaseScope('db');
  const kernel = createReadReceiptsKernel({ baselineMs: 1_000 });
  const { wrap } = readReceiptsFixture(kernel);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = ['Recent', 'Older'].map((title, i) => ({ id: `t${i}`, areaId: area.id, title, sort: i,
    cwd: '/tmp', agentCwd: '/tmp', pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 2, ...NEUTRAL_ACTIVITY }));
  const onGo = vi.fn();
  const onSetPinned = vi.fn();
  render(wrap(<UiPreferencesProvider preferences={preferences}>
    <ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
      <div className={`${styles.shell} ${styles.shellExpanded}`} style={{ blockSize: '100dvh' }}>
        <Sidebar areas={[area]} tracksByArea={new Map([[area.id, tracks]])} tracks={tracks}
          currentPath="/" onGo={onGo} onRequestCreateArea={vi.fn()} onRequestEditArea={vi.fn()}
          onDeleteArea={vi.fn()} onNewTrack={vi.fn()} onSetPinned={onSetPinned} onDeleteTrack={vi.fn()}
          onOpenSettings={vi.fn()} onOpenPlugins={vi.fn()} onSignOut={vi.fn()}
          collapsed={false} onToggleCollapsed={vi.fn()} />
        <main />
      </div>
    </ThemeProvider>
  </UiPreferencesProvider>));
  const menu = page.getByRole('button', { name: 'Actions for track Older' });
  await expect.element(menu).toBeVisible();
  const row = page.getByRole('button', { name: 'Track Older', exact: true }).element();
  await page.elementLocator(row).hover();
  expect(menu.element().getBoundingClientRect().right).toBeLessThanOrEqual(row.getBoundingClientRect().right);
  await menu.click();
  await page.getByRole('menuitem', { name: 'Pin globally', exact: true }).click();
  expect(onSetPinned).toHaveBeenCalledWith('t1', true);
  await menu.click();
  await page.getByRole('menuitem', { name: 'Pin within area', exact: true }).click();
  const areaGroup = page.getByRole('group', { name: 'area Work', exact: true });
  const navigationRows = [...areaGroup.element().querySelectorAll<HTMLButtonElement>('button[aria-label^="Track "]')];
  expect(navigationRows[0]?.getAttribute('aria-label')).toBe('Track Older');
  await menu.click();
  await page.getByRole('menuitem', { name: 'Mark as unread', exact: true }).click();
  await expect.poll(() => kernel.stored('track', 't1')?.manualUnread).toBe(true);
  await expect.poll(() => row.getAttribute('aria-describedby')).toBeTruthy();
  await menu.click();
  await userEvent.keyboard('{Escape}');
  await expect.element(menu).toHaveFocus();
  expect(onGo).not.toHaveBeenCalled();
});


it('reserves room for menu actions and metadata in Today compact rows', async () => {
  await page.viewport(1400, 900);
  const track: Track = { id: 't', title: 'Compact row', areaId: 'a', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY };
  render(<ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
    <div style={{ inlineSize: '20rem' }}><TrackRow track={track} variant="compact" nowMs={10_000}
      onOpen={vi.fn()} onDelete={vi.fn()}
      actions={{ areaPinned: false, onSetPinned: vi.fn(), onSetAreaPinned: vi.fn(), onMarkUnread: vi.fn() }} /></div>
  </ThemeProvider>);
  const row = page.getByRole('button', { name: 'Track Compact row', exact: true });
  await row.hover();
  const remove = page.getByRole('button', { name: 'Delete Compact row', exact: true }).element().getBoundingClientRect();
  const age = row.element().lastElementChild!.getBoundingClientRect();
  const menu = page.getByRole('button', { name: 'Actions for track Compact row', exact: true }).element().getBoundingClientRect();
  expect(age.right).toBeLessThanOrEqual(menu.left);
  expect(menu.right).toBeLessThanOrEqual(remove.left);
});


it('fades only overflowing rail titles and keeps the action menu distinct', async () => {
  await page.viewport(1400, 900);
  const base: Track = { id: 'long', title: '', areaId: 'a', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY };
  const tree = (title: string, width: string) => <ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
    <div style={{ inlineSize: width }}><TrackRow track={{ ...base, title }} variant="rail" presentation={{ kind: 'entry', depth: 0 }}
      onOpen={vi.fn()} onDelete={vi.fn()}
      actions={{ areaPinned: false, onSetPinned: vi.fn(), onSetAreaPinned: vi.fn(), onMarkUnread: vi.fn() }} /></div>
  </ThemeProvider>;
  const english = 'Review the exceptionally long navigation title';
  const chinese = '验证很长的任务标题与右侧操作菜单是否存在视觉冲突';
  const view = render(tree(chinese, '14rem'));
  const titleNode = () => document.querySelector<HTMLElement>('button[data-nc-role="row"] [title]')!;
  for (const title of [chinese, english]) {
    view.rerender(tree(title, '14rem'));
    await page.getByRole('button', { name: `Track ${title}`, exact: true }).hover();
    expect(titleNode().scrollWidth).toBeGreaterThan(titleNode().clientWidth);
    await expect.poll(() => getComputedStyle(titleNode()).maskImage).toContain('linear-gradient');
    expect(getComputedStyle(titleNode()).textOverflow).toBe('clip');
    expect(page.getByRole('button', { name: `Track ${title}`, exact: true }).element().getAttribute('aria-label')).toBe(`Track ${title}`);
    const menu = page.getByRole('button', { name: `Actions for track ${title}`, exact: true }).element();
    expect(titleNode().getBoundingClientRect().right).toBeLessThanOrEqual(menu.getBoundingClientRect().left);
  }
  view.rerender(tree('Short', '14rem'));
  await expect.poll(() => getComputedStyle(titleNode()).maskImage).toBe('none');
  expect(titleNode().scrollWidth).toBeLessThanOrEqual(titleNode().clientWidth);
  view.rerender(tree(english, '14rem'));
  await expect.poll(() => getComputedStyle(titleNode()).maskImage).toContain('linear-gradient');
  // A layout resize without changing React props must also remove the fade.
  titleNode().closest('button')!.parentElement!.parentElement!.style.inlineSize = '40rem';
  await expect.poll(() => getComputedStyle(titleNode()).maskImage).toBe('none');
});


it('reveals Track controls on hover or focus and lets the title use idle space', async () => {
  await page.viewport(1400, 900);
  const title = 'A long navigation title used to verify space for hover actions';
  const track: Track = { id: 'hover', title, areaId: 'a', sort: 0, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 1, ...NEUTRAL_ACTIVITY };
  render(<ThemeProvider storage={{ getItem: () => 'light', setItem: () => undefined }}>
    <div style={{ inlineSize: '14rem' }}><TrackRow track={track} variant="rail" presentation={{ kind: 'entry', depth: 0 }} unread onOpen={vi.fn()} onDelete={vi.fn()}
      actions={{ areaPinned: false, onSetPinned: vi.fn(), onSetAreaPinned: vi.fn(), onMarkUnread: vi.fn() }} /></div>
    <button type="button">Outside row</button>
  </ThemeProvider>);
  const outside = page.getByRole('button', { name: 'Outside row', exact: true });
  await outside.hover();
  const row = page.getByRole('button', { name: `Track ${title}`, exact: true });
  const menu = page.getByRole('button', { name: `Actions for track ${title}`, exact: true });
  const remove = page.getByRole('button', { name: `Delete ${title}`, exact: true });
  const caption = row.element().querySelector<HTMLElement>('[title]')!;
  await expect.poll(() => getComputedStyle(menu.element()).opacity).toBe('0');
  await expect.poll(() => getComputedStyle(remove.element()).opacity).toBe('0');
  const idleWidth = caption.clientWidth;
  await row.hover();
  await expect.poll(() => getComputedStyle(menu.element()).opacity).toBe('1');
  await expect.poll(() => getComputedStyle(remove.element()).opacity).toBe('1');
  expect(caption.clientWidth).toBeLessThan(idleWidth);
  expect(caption.getBoundingClientRect().right).toBeLessThanOrEqual(menu.element().getBoundingClientRect().left);
  await outside.click();
  await expect.poll(() => getComputedStyle(menu.element()).opacity).toBe('0');
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Shift>}{Tab}{/Shift}');
  await expect.element(menu).toHaveFocus();
  await expect.poll(() => getComputedStyle(menu.element()).opacity).toBe('1');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('menuitem', { name: 'Pin globally', exact: true })).toHaveFocus();
  expect(menu.element().getAttribute('aria-expanded')).toBe('true');
  const status = row.element().parentElement!.querySelector('[data-nc-activity]')!.parentElement!;
  await expect.poll(() => getComputedStyle(status).opacity).toBe('1');
  expect(getComputedStyle(menu.element()).opacity).toBe('1');
  await userEvent.keyboard('{Escape}');
  await expect.element(menu).toHaveFocus();
});

function textBounds(node: Element) {
  const range = document.createRange();
  range.selectNodeContents(node);
  return range.getBoundingClientRect();
}

it('aligns Area and flat entry text in one schema-owned column and type', async () => {
  await scene(true);
  const area = page.getByRole('button', { name: 'Collapse area Work', exact: true }).element().querySelector('[title]')!;
  const flat = page.getByRole('group', { name: 'Waiting on you', exact: true }).element().querySelector('[title="Review result"]')!;
  const font = (node: Element) => {
    const style = getComputedStyle(node);
    return [style.fontFamily, style.fontSize, style.fontWeight, style.lineHeight, style.letterSpacing];
  };
  const rail = page.getByRole('navigation', { name: 'Workspace' }).element();
  for (const width of ['15rem', '11rem']) {
    rail.style.inlineSize = width;
    expect.soft(textBounds(area).left).toBeCloseTo(textBounds(flat).left, 1);
    expect.soft(font(area)).toEqual(font(flat));
  }
});
