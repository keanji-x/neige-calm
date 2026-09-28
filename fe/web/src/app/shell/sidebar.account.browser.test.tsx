import { cleanup, render } from '@testing-library/react';
import { page } from 'vitest/browser';
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

function renderRail(theme: 'light' | 'dark') {
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
          collapsed={false}
          onToggleCollapsed={vi.fn()}
          userLabel="Kenji Xie"
        />
      </div>
    </ThemeProvider>,
  );
  const rail = document.querySelector<HTMLElement>('nav[aria-label="Workspace"]')!;
  const avatar = document.querySelector<HTMLElement>('[aria-label="Account menu for Kenji Xie"]')!;
  return { rail, avatar };
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
