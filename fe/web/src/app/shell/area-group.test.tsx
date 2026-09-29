// @vitest-environment jsdom
// An expanded Area shows its five most recent Tracks, the open one, and `Show N more`.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); });

function area(overrides: Partial<Area> = {}): Area {
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
}

/** `count` Tracks in recent-first order, titled T1…Tn. */
function tracks(count: number, areaId = 'c1', prefix = 'T'): Track[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `${areaId}-${index + 1}`, areaId, title: `${prefix}${index + 1}`, sort: index,
    cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
  }));
}

type View = Readonly<{
  areas?: readonly Area[];
  byArea?: ReadonlyMap<string, readonly Track[]>;
  currentPath?: string;
  collapsed?: boolean;
}>;

function renderRail(initial: View) {
  const preferences = createUiPreferences();
  const build = (view: View) => {
    const areas = view.areas ?? [area()];
    const byArea = view.byArea ?? new Map();
    return (
      <UiPreferencesProvider preferences={preferences}>
        <Sidebar
          areas={areas}
          tracksByArea={byArea}
          tracks={[...byArea.values()].flat()}
          currentPath={view.currentPath ?? '/'}
          onGo={vi.fn()}
          onRequestCreateArea={vi.fn()}
          onRequestEditArea={vi.fn()}
          onDeleteArea={vi.fn()}
          onNewTrack={vi.fn()}
          onSetPinned={vi.fn()}
          onDeleteTrack={vi.fn()}
          collapsed={view.collapsed ?? false}
          onToggleCollapsed={vi.fn()}
          onOpenSettings={vi.fn()}
          onOpenPlugins={vi.fn()}
          onSignOut={vi.fn()}
        />
      </UiPreferencesProvider>
    );
  };
  const result = render(build(initial));
  return { update: (view: View) => result.rerender(build(view)) };
}

function work(count: number): ReadonlyMap<string, readonly Track[]> {
  return new Map([['c1', tracks(count)]]);
}

/** Titles of the Track rows the rail shows, in order. */
function shownTitles(): string[] {
  return screen.queryAllByRole('button', { name: /^Track / })
    .map((row) => row.getAttribute('aria-label') ?? row.textContent ?? '')
    .map((name) => /^Track ([^\s,]+)/.exec(name)?.[1] ?? name);
}

function toggle(): HTMLElement | null {
  return screen.queryByRole('button', { name: /^Show (less|\d+ more) in / });
}

describe('Area track limit', () => {
  it('shows five, reveals all seven, and goes back to five from the same button', async () => {
    renderRail({ byArea: work(7) });
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5']);
    const button = screen.getByRole('button', { name: 'Show 2 more in Work' });
    expect(button.textContent).toBe('Show 2 more');
    expect(button.hasAttribute('aria-expanded')).toBe(false);

    await userEvent.click(button);
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5', 'T6', 'T7']);
    expect(toggle()).toBe(button);
    expect(button.textContent).toBe('Show less');
    expect(button.getAttribute('aria-label')).toBe('Show less in Work');
    expect(document.activeElement).toBe(button);

    await userEvent.click(button);
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5']);
    expect(button.textContent).toBe('Show 2 more');
    expect(document.activeElement).toBe(button);
  });

  it('has no button at exactly five', () => {
    renderRail({ byArea: work(5) });
    expect(shownTitles()).toHaveLength(5);
    expect(toggle()).toBeNull();
  });

  it('counts one hidden Track at six', () => {
    renderRail({ byArea: work(6) });
    expect(shownTitles()).toHaveLength(5);
    expect(toggle()?.textContent).toBe('Show 1 more');
  });

  it('shows an open sixth Track with nothing left to reveal', () => {
    renderRail({ byArea: work(6), currentPath: '/track/c1-6' });
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5', 'T6']);
    expect(toggle()).toBeNull();
  });

  it('keeps an open seventh Track last and does not count it as hidden', () => {
    renderRail({ byArea: work(7), currentPath: '/track/c1-7' });
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5', 'T7']);
    expect(screen.getByRole('button', { name: /^Track T7/ }).getAttribute('aria-current')).toBe('page');
    expect(toggle()?.textContent).toBe('Show 1 more');
  });

  it('does not repeat an open Track that is already among the five', () => {
    renderRail({ byArea: work(7), currentPath: '/track/c1-3' });
    expect(shownTitles()).toEqual(['T1', 'T2', 'T3', 'T4', 'T5']);
    expect(screen.getByRole('button', { name: /^Track T3/ }).getAttribute('aria-current')).toBe('page');
    expect(toggle()?.textContent).toBe('Show 2 more');
  });

  it('keeps Show all across collapsing and re-expanding the Area', async () => {
    renderRail({ byArea: work(7) });
    await userEvent.click(screen.getByRole('button', { name: 'Show 2 more in Work' }));
    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(shownTitles()).toEqual([]);
    await userEvent.click(screen.getByRole('button', { name: 'Expand area Work' }));
    expect(shownTitles()).toHaveLength(7);
    expect(toggle()?.textContent).toBe('Show less');
  });

  it('goes back to five after the whole rail collapses and expands', async () => {
    const view = renderRail({ byArea: work(7) });
    await userEvent.click(screen.getByRole('button', { name: 'Show 2 more in Work' }));
    view.update({ byArea: work(7), collapsed: true });
    view.update({ byArea: work(7), collapsed: false });
    expect(shownTitles()).toHaveLength(5);
    expect(toggle()?.textContent).toBe('Show 2 more');
  });

  it('names each Area\'s button after its Area', async () => {
    const reading = area({ id: 'c2', name: 'Reading', sort: 2 });
    renderRail({
      areas: [area(), reading],
      byArea: new Map([['c1', tracks(7)], ['c2', tracks(8, 'c2', 'R')]]),
    });
    expect(screen.getByRole('button', { name: 'Show 2 more in Work' })).toBeTruthy();
    const readingMore = screen.getByRole('button', { name: 'Show 3 more in Reading' });
    await userEvent.click(readingMore);
    expect(screen.getByRole('button', { name: 'Show less in Reading' })).toBe(readingMore);
    expect(screen.getByRole('button', { name: 'Show 2 more in Work' })).toBeTruthy();
  });

  it('keeps Show all while the Area shrinks to five and grows again', async () => {
    const view = renderRail({ byArea: work(7) });
    await userEvent.click(screen.getByRole('button', { name: 'Show 2 more in Work' }));
    view.update({ byArea: work(5) });
    expect(shownTitles()).toHaveLength(5);
    expect(toggle()).toBeNull();
    view.update({ byArea: work(8) });
    expect(shownTitles()).toHaveLength(8);
    expect(toggle()?.textContent).toBe('Show less');
  });

  it('scrolls the button into view after Show less, and not on mount or Show more', async () => {
    const scrolled: HTMLElement[] = [];
    const scrollIntoView = vi.fn(function recordScroll(this: HTMLElement) {
      scrolled.push(this);
    });
    const originalScrollIntoView = Object.getOwnPropertyDescriptor(
      HTMLElement.prototype, 'scrollIntoView',
    );
    Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', {
      configurable: true,
      value: scrollIntoView,
    });
    try {
      renderRail({ byArea: work(7) });
      expect(scrollIntoView).not.toHaveBeenCalled();
      const button = screen.getByRole('button', { name: 'Show 2 more in Work' });
      await userEvent.click(button);
      expect(scrollIntoView).not.toHaveBeenCalled();
      await userEvent.click(button);
      expect(scrolled).toEqual([button]);
      expect(scrollIntoView).toHaveBeenLastCalledWith({ block: 'nearest' });
    } finally {
      if (originalScrollIntoView === undefined) {
        Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
      } else {
        Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', originalScrollIntoView);
      }
    }
  });
});
