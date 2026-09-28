import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import '../../styles/entry.css';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

const AREAS: readonly Area[] = Object.freeze(Array.from({ length: 40 }, (_, index) => ({
  id: `a${index}`, name: `Area ${index}`, color: '#5B8DEF', sort: index, kind: 'user' as const,
  defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
})));

function renderRail(theme: 'light' | 'dark', { collapsed = false, mainLayer = false } = {}) {
  render(
    <ThemeProvider storage={{ getItem: () => theme, setItem: () => undefined }}>
      <div style={{ display: 'flex', flexDirection: 'column', blockSize: '480px' }}>
        <Sidebar
          areas={AREAS}
          tracksByArea={new Map(AREAS.map((area) => [area.id, []]))}
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
          collapsed={collapsed}
          onToggleCollapsed={vi.fn()}
          userLabel="Kenji Xie"
        />
      </div>
      {/* Stands in for `.main`: a later sibling carrying the highest rank its
          content uses in place (the sticky headers at `--z-sticky`). */}
      {mainLayer && (
        <div
          data-testid="main-layer"
          style={{ position: 'fixed', inset: 0, zIndex: 'var(--z-sticky)' }}
        />
      )}
    </ThemeProvider>,
  );
  const rail = document.querySelector<HTMLElement>('nav[aria-label="Workspace"]')!;
  const avatar = document.querySelector<HTMLElement>('[aria-label="Account menu for Kenji Xie"]')!;
  let userRow = avatar;
  while (userRow.parentElement !== rail) userRow = userRow.parentElement!;
  return { rail, avatar, userRow };
}

it('keeps the account avatar pinned to the bottom of the rail however many areas scroll above it', async () => {
  await page.viewport(1400, 900);
  const { rail, avatar } = renderRail('light');

  expect(rail.scrollHeight).toBeGreaterThan(rail.clientHeight);
  const bottomGap = () => rail.getBoundingClientRect().bottom - avatar.getBoundingClientRect().bottom;
  const atTop = bottomGap();
  expect(atTop).toBeGreaterThanOrEqual(0);
  expect(avatar.getBoundingClientRect().top).toBeGreaterThanOrEqual(rail.getBoundingClientRect().top);

  rail.scrollTop = rail.scrollHeight;
  await new Promise(requestAnimationFrame);
  expect(bottomGap()).toBe(atTop);

  // Nothing scrolling underneath paints over it: the topmost element at the
  // avatar's centre is the avatar itself.
  const box = avatar.getBoundingClientRect();
  expect(document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2)).toBe(avatar);
});

it('covers the area rows scrolling underneath the account row, action buttons included', async () => {
  await page.viewport(1400, 900);
  const { rail, userRow } = renderRail('light');

  // Halfway down, area rows sit behind the row rather than above it.
  rail.scrollTop = (rail.scrollHeight - rail.clientHeight) / 2;
  await new Promise(requestAnimationFrame);
  const row = userRow.getBoundingClientRect();
  const under = [...rail.querySelectorAll<HTMLElement>('[aria-label^="Area actions for"]')].filter((button) => {
    const box = button.getBoundingClientRect();
    return box.top < row.bottom && box.bottom > row.top;
  });
  expect(under.length).toBeGreaterThan(0);

  // Hit-testing every overlapped action button's centre lands in the account
  // row: the `--z-raised` buttons do not climb over it.
  for (const button of under) {
    const box = button.getBoundingClientRect();
    const hit = document.elementFromPoint(box.left + box.width / 2,
      Math.min(Math.max(box.top + box.height / 2, row.top + 1), row.bottom - 1));
    expect(userRow.contains(hit)).toBe(true);
  }
  // Hit-testing ignores paint, so the fill that hides the rows is read directly.
  expect(getComputedStyle(userRow).backgroundColor).toBe(getComputedStyle(rail).backgroundColor);
});

it('keeps the account menu above the main region when it overhangs the collapsed strip', async () => {
  await page.viewport(1400, 900);
  const { avatar } = renderRail('light', { collapsed: true, mainLayer: true });

  await userEvent.click(avatar);
  const items = page.getByRole('menuitem').elements() as HTMLElement[];
  expect(items.map((item) => item.textContent)).toEqual(['Settings', 'Plugins', 'Sign out']);
  const main = document.querySelector<HTMLElement>('[data-testid="main-layer"]')!;
  for (const item of items) {
    const box = item.getBoundingClientRect();
    // The item reaches past the 44px strip, over where `.main` is.
    expect(box.right).toBeGreaterThan(44);
    const hit = document.elementFromPoint(box.right - 4, box.top + box.height / 2);
    expect(hit).not.toBe(main);
    expect(item.contains(hit)).toBe(true);
  }
});

it.each(['light', 'dark'] as const)('paints the avatar with the card fill, not the rail fill (%s)', async (theme) => {
  await page.viewport(1400, 900);
  const { rail, avatar } = renderRail(theme);
  expect(document.documentElement.dataset.theme).toBe(theme);
  const rootStyle = getComputedStyle(document.documentElement);
  const probe = document.createElement('div');
  document.body.append(probe);
  probe.style.background = rootStyle.getPropertyValue('--surface-card');
  const cardFill = getComputedStyle(probe).backgroundColor;
  probe.remove();

  expect(getComputedStyle(avatar).backgroundColor).toBe(cardFill);
  expect(getComputedStyle(avatar).backgroundColor).not.toBe(getComputedStyle(rail).backgroundColor);
});
