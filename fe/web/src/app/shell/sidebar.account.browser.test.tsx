import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import '../../styles/entry.css';
import { useState } from '../../ui/state/public.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

const VIEWPORT_H = 480;

const AREAS: readonly Area[] = Object.freeze(Array.from({ length: 40 }, (_, index) => ({
  id: `a${index}`, name: `Area ${index}`, color: '#5B8DEF', sort: index, kind: 'user' as const,
  defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1,
})));

const LAST_TRACK: Track = Object.freeze({
  id: 't39', areaId: 'a39', title: 'Deep track', sort: 1, lifecycle: 'draft' as const, cwd: '/tmp',
  archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
  ...NEUTRAL_ACTIVITY,
});

type RailOptions = { theme?: 'light' | 'dark'; collapsed?: boolean; mainLayer?: boolean; currentPath?: string };

function Rail({ theme, collapsed: initiallyCollapsed, mainLayer, currentPath }: Required<RailOptions>) {
  const [collapsed, setCollapsed] = useState(initiallyCollapsed);
  const tracks = [LAST_TRACK];
  return (
    <ThemeProvider storage={{ getItem: () => theme, setItem: () => undefined }}>
      <div style={{ display: 'flex', flexDirection: 'column', blockSize: '100dvh' }}>
        <Sidebar
          areas={AREAS}
          tracksByArea={new Map(AREAS.map((area) => [area.id, tracks.filter((track) => track.areaId === area.id)]))}
          tracks={tracks}
          currentPath={currentPath}
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
          onToggleCollapsed={() => setCollapsed((value) => !value)}
          userLabel="Kenji Xie"
        />
      </div>
      {/* Stands in for `.main`: a later sibling carrying the highest rank its
          content uses in place (the sticky headers at `--z-sticky`). */}
      {mainLayer && (
        <div data-testid="main-layer" style={{ position: 'fixed', inset: 0, zIndex: 'var(--z-sticky)' }} />
      )}
    </ThemeProvider>
  );
}

async function renderRail({ theme = 'light', collapsed = false, mainLayer = false, currentPath = '/' }: RailOptions = {}) {
  await page.viewport(1400, VIEWPORT_H);
  render(<Rail theme={theme} collapsed={collapsed} mainLayer={mainLayer} currentPath={currentPath} />);
  await new Promise(requestAnimationFrame);
  return railParts();
}

function railParts() {
  const rail = document.querySelector<HTMLElement>('nav[aria-label="Workspace"]')!;
  const avatar = document.querySelector<HTMLElement>('[aria-label="Account menu for Kenji Xie"]')!;
  let userRow = avatar;
  while (userRow.parentElement !== rail) userRow = userRow.parentElement!;
  return { rail, avatar, userRow };
}

async function scrollRail(rail: HTMLElement, top: number) {
  rail.scrollTop = top;
  await new Promise(requestAnimationFrame);
}

/** The element's centre is the topmost thing there: nothing covers it. */
function expectUncovered(element: HTMLElement) {
  const box = element.getBoundingClientRect();
  const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
  const name = element.getAttribute('aria-label') ?? element.textContent;
  expect(element.contains(hit), `${name} is covered by ${hit?.outerHTML.slice(0, 80)}`).toBe(true);
}

it('keeps the account avatar pinned where it sat, with the row fill reaching the rail bottom, at every scroll', async () => {
  const { rail, avatar, userRow } = await renderRail();
  expect(rail.scrollHeight).toBeGreaterThan(rail.clientHeight);
  // Before the row was pinned, the avatar sat one rail bottom padding above
  // the rail's bottom edge. Pinning must not move it.
  const railPadding = parseFloat(getComputedStyle(rail).paddingBlockEnd);
  expect(railPadding).toBeGreaterThan(0);

  for (const top of [0, Math.round((rail.scrollHeight - rail.clientHeight) / 2), rail.scrollHeight]) {
    await scrollRail(rail, top);
    const railBottom = rail.getBoundingClientRect().bottom;
    // Within a pixel: the rail's top inset is fractional, so layout snaps.
    expect(Math.abs(railBottom - avatar.getBoundingClientRect().bottom - railPadding)).toBeLessThanOrEqual(1);
    expect(Math.abs(userRow.getBoundingClientRect().bottom - railBottom)).toBeLessThanOrEqual(1);
    expectUncovered(avatar);
  }
});

it('covers the area rows scrolling underneath the account row, action buttons included', async () => {
  const { rail, userRow } = await renderRail();

  // Halfway down, area rows sit behind the row rather than above it.
  await scrollRail(rail, Math.round((rail.scrollHeight - rail.clientHeight) / 2));
  const row = userRow.getBoundingClientRect();
  const railBox = rail.getBoundingClientRect();
  const under = [...rail.querySelectorAll<HTMLElement>('[aria-label^="Area actions for"]')].filter((button) => {
    const box = button.getBoundingClientRect();
    return box.top < row.bottom && box.bottom > row.top;
  });
  expect(under.length).toBeGreaterThan(0);

  // Every point of the band from the row's top to the rail's bottom edge
  // hit-tests into the row: no row shows through below or beside the avatar,
  // and the `--z-raised` action buttons do not climb over it.
  const xs = [railBox.left + 60, ...under.map((button) => button.getBoundingClientRect().left + 4)];
  for (let y = row.top + 1; y < railBox.bottom - 1; y += 2) {
    for (const x of xs) {
      expect(userRow.contains(document.elementFromPoint(x, y)), `(${x}, ${y})`).toBe(true);
    }
  }
  // Hit-testing ignores paint, so the fill that hides the rows is read directly.
  expect(getComputedStyle(userRow).backgroundColor).toBe(getComputedStyle(rail).backgroundColor);
});

it.each([false, true])('opens the account menu clear of the avatar and above the main region (collapsed: %s)', async (collapsed) => {
  const { avatar } = await renderRail({ collapsed, mainLayer: true });

  await userEvent.click(avatar);
  const items = page.getByRole('menuitem').elements() as HTMLElement[];
  expect(items.map((item) => item.textContent)).toEqual(['Settings', 'Plugins', 'Sign out']);
  const menu = items[0]?.closest<HTMLElement>('[role="menu"]');
  if (!menu) throw new Error('account menu did not open');
  // Within a pixel of snapping; an avatar pushed up under the menu overlaps by a whole rail padding.
  expect(menu.getBoundingClientRect().bottom).toBeLessThanOrEqual(avatar.getBoundingClientRect().top + 1);
  expectUncovered(avatar);
  for (const item of items) {
    expectUncovered(item);
    // In the 44px strip the item reaches past the rail, over `.main`.
    if (collapsed) {
      const box = item.getBoundingClientRect();
      expect(box.right).toBeGreaterThan(44);
      expect(item.contains(document.elementFromPoint(box.right - 4, box.top + box.height / 2))).toBe(true);
    }
  }
});

it('reveals the current track above the account row, not behind it', async () => {
  await renderRail({ currentPath: `/track/${LAST_TRACK.id}` });
  const current = document.querySelector<HTMLElement>('[aria-current="page"]')!;
  expect(current.textContent).toContain(LAST_TRACK.title);
  expectUncovered(current);
});

it('keeps every control reached by Tab clear of the account row', async () => {
  const { avatar } = await renderRail();
  document.querySelector<HTMLElement>('[aria-label="New area"]')!.focus();
  let checked = 0;
  for (let step = 0; step < 400 && document.activeElement !== avatar; step += 1) {
    await userEvent.tab();
    await new Promise(requestAnimationFrame);
    const focused = document.activeElement as HTMLElement;
    if (focused !== avatar) { expectUncovered(focused); checked += 1; }
  }
  expect(document.activeElement).toBe(avatar);
  expect(checked).toBeGreaterThan(AREAS.length);
});

it('restores focus from a collapsed-strip initial onto a disclosure the account row does not cover', async () => {
  await renderRail({ collapsed: true });
  await userEvent.click(page.getByRole('button', { name: `Show area ${AREAS.at(-1)!.name}` }));
  await new Promise(requestAnimationFrame);
  const focused = document.activeElement as HTMLElement;
  expect(focused.getAttribute('aria-label') ?? '').toContain(AREAS.at(-1)!.name);
  expectUncovered(focused);
});

it.each(['light', 'dark'] as const)('paints the avatar with the card fill, not the rail fill (%s)', async (theme) => {
  const { rail, avatar } = await renderRail({ theme });
  expect(document.documentElement.dataset.theme).toBe(theme);
  const probe = document.createElement('div');
  document.body.append(probe);
  probe.style.background = getComputedStyle(document.documentElement).getPropertyValue('--surface-card');
  const cardFill = getComputedStyle(probe).backgroundColor;
  probe.remove();

  expect(getComputedStyle(avatar).backgroundColor).toBe(cardFill);
  expect(getComputedStyle(avatar).backgroundColor).not.toBe(getComputedStyle(rail).backgroundColor);
});
