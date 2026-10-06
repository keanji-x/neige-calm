import { act, cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import styles from './shell.module.css';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

it('places view options before collapse and shares group disclosure, keyboard actions and live membership', async () => {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
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
  const sidebar = (tracks: Track[]) => <UiPreferencesProvider preferences={preferences}>
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
  </UiPreferencesProvider>;
  const view = render(sidebar(tracks));

  const areasTitle = page.getByRole('button', { name: 'Collapse Areas', exact: true }).element()
    .querySelector<HTMLElement>('[title="Areas"]')!;
  const areaTitle = page.getByRole('button', { name: 'Collapse area Work', exact: true }).element()
    .querySelector<HTMLElement>('[title="Work"]')!;
  expect(areaTitle.getBoundingClientRect().left).toBeGreaterThan(areasTitle.getBoundingClientRect().left);

  const options = page.getByRole('button', { name: 'Sidebar view options' });
  const collapse = page.getByRole('button', { name: 'Collapse sidebar' });
  const today = page.getByRole('button', { name: 'Go to Today' });
  const areasDisclosure = page.getByRole('button', { name: 'Collapse Areas', exact: true });
  const areasMarker = areasDisclosure.element().querySelector<HTMLElement>('span[aria-hidden="true"]')!;
  await today.hover();
  expect(getComputedStyle(areasMarker).opacity).toBe('0');
  await areasDisclosure.hover();
  expect(getComputedStyle(areasMarker).opacity).toBe('1');
  await areasDisclosure.click();
  await today.hover();
  expect(getComputedStyle(areasMarker).opacity).toBe('1');
  await page.getByRole('button', { name: 'Expand Areas', exact: true }).click();
  await today.hover();
  expect(getComputedStyle(areasMarker).opacity).toBe('0');
  await userEvent.keyboard('{Shift>}{Tab}{/Shift}{Tab}');
  await expect.element(areasDisclosure).toHaveFocus();
  expect(getComputedStyle(areasMarker).opacity).toBe('1');
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
  await today.hover();
  const pinNext = page.getByRole('button', { name: 'Pin Next task' });
  expect(getComputedStyle(pinNext.element()).opacity).toBe('0');
  expect(getComputedStyle(pinNext.element().querySelector('svg')!).rotate).toBe('none');
  const pins = page.getByRole('button', { name: 'Unpin Build frontend' }).elements() as HTMLElement[];
  expect(pins).toHaveLength(4);
  for (const pin of pins) {
    expect(getComputedStyle(pin).opacity).toBe('0');
    expect(getComputedStyle(pin).pointerEvents).toBe('none');
    expect(getComputedStyle(pin.querySelector('svg')!).rotate).toBe('180deg');
  }
  const runningGroup = page.getByRole('group', { name: 'Running', exact: true });
  const runningRow = runningGroup.getByRole('button', { name: /^Track Build frontend/ });
  const unpin = runningGroup.getByRole('button', { name: 'Unpin Build frontend' });
  await runningRow.hover();
  await expect.poll(() => getComputedStyle(unpin.element()).opacity).toBe('1');
  expect(getComputedStyle(unpin.element()).pointerEvents).toBe('auto');
  await today.hover();
  unpin.element().focus();
  await expect.poll(() => getComputedStyle(unpin.element()).opacity).toBe('1');
  await unpin.click();
  expect(onSetPinned).toHaveBeenCalledWith('running', false);
  await page.getByRole('group', { name: 'Running', exact: true }).getByRole('button', { name: /^Track Build frontend/ }).click();
  expect(onGo).toHaveBeenCalledWith({ name: 'track', trackId: 'running' });
  expect(document.querySelectorAll('[aria-current="page"]')).toHaveLength(1);
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
    await page.getByRole('menuitem', { name: 'Hidden groups' }).click();
    await page.getByRole('menuitem', { name: `Show ${title}`, exact: true }).click();
    await expect.element(page.getByRole('group', { name: title, exact: true })).toBeVisible();
  }
  act(() => { preferences.markRead('track', 'review', 10); preferences.markRead('track', 'running', 10); });
  await expect.element(page.getByRole('group', { name: 'Unread', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Unread', exact: true }).getByRole('button', { name: /^Track / })).not.toBeInTheDocument();
  await expect.element(page.getByRole('group', { name: 'Waiting on you', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Running', exact: true })).toBeVisible();
  view.rerender(sidebar(tracks.map((track) => ({ ...track, working: false }))));
  await expect.element(page.getByRole('group', { name: 'Running', exact: true })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Running', exact: true }).getByRole('button', { name: /^Track / })).not.toBeInTheDocument();
});


it('uses the three-dot Track menu with keyboard and keeps actions separate from navigation', async () => {
  await page.viewport(1400, 900);
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1_000);
  const area: Area = { id: 'work', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 };
  const tracks: Track[] = ['Recent', 'Older'].map((title, i) => ({ id: `t${i}`, areaId: area.id, title, sort: i,
    cwd: '/tmp', agentCwd: '/tmp', pinnedAt: null, closedAt: null, createdAt: 1, updatedAt: 2, ...NEUTRAL_ACTIVITY }));
  const onGo = vi.fn();
  const onSetPinned = vi.fn();
  render(<UiPreferencesProvider preferences={preferences}>
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
  </UiPreferencesProvider>);
  const menu = page.getByRole('button', { name: 'Actions for track Older' });
  await expect.element(menu).toBeVisible();
  const row = page.getByRole('button', { name: 'Track Older', exact: true }).element();
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
  expect(preferences.isUnread('track', 't1', 0)).toBe(true);
  expect(row.getAttribute('aria-describedby')).toBeTruthy();
  await menu.click();
  await userEvent.keyboard('{Escape}');
  await expect.element(menu).toHaveFocus();
  expect(onGo).not.toHaveBeenCalled();
});
