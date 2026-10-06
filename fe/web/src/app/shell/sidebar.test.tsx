// @vitest-environment jsdom
// Behaviour of the workspace rail: disclosure, badges, create/delete, account menu.
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { createUiPreferences, UiPreferencesProvider } from '../providers/ui-preferences.tsx';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

function area(overrides: Partial<Area> = {}): Area {
  return {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user',
    defaultTemplateId: null, defaultCwd: null, createdAt: 0, updatedAt: 0, ...overrides,
  };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Task', sort: 1, cwd: '/tmp', agentCwd: '/tmp',
    pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

type Props = Parameters<typeof Sidebar>[0];

function renderSidebar(props: Partial<Props> = {}, preferences = createUiPreferences()) {
  const build = (overrides: Partial<Props>) => {
    const merged = { ...props, ...overrides };
    const tracks = merged.tracks ?? [];
    return (
      <UiPreferencesProvider preferences={preferences}><ThemeProvider storage={memoryStorage()}>
        <Sidebar
          areas={merged.areas ?? [area()]}
          tracksByArea={merged.tracksByArea ?? new Map([['c1', tracks]])}
          tracks={tracks}
          currentPath={merged.currentPath ?? '/'}
          onGo={merged.onGo ?? vi.fn()}
          onRequestCreateArea={merged.onRequestCreateArea ?? vi.fn()}
          onRequestEditArea={merged.onRequestEditArea ?? vi.fn()}
          onDeleteArea={merged.onDeleteArea ?? vi.fn()}
          onNewTrack={merged.onNewTrack ?? vi.fn()}
          onSetPinned={merged.onSetPinned ?? vi.fn()}
          onDeleteTrack={merged.onDeleteTrack ?? vi.fn()}
          collapsed={merged.collapsed ?? false}
          onToggleCollapsed={merged.onToggleCollapsed ?? vi.fn()}
          onOpenSettings={merged.onOpenSettings ?? vi.fn()}
          onOpenPlugins={merged.onOpenPlugins ?? vi.fn()}
          onSignOut={merged.onSignOut ?? vi.fn()}
          userLabel={merged.userLabel}
          readError={merged.readError}
          activityError={merged.activityError}
          readLoading={merged.readLoading}
          onRetryRead={merged.onRetryRead}
        />
      </ThemeProvider></UiPreferencesProvider>
    );
  };
  const result = render(build({}));
  return { ...result, update: (overrides: Partial<Props>) => result.rerender(build(overrides)) };
}

async function requestAreaDelete(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: 'Area actions for Work' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Delete area' }));
}

describe('workspace read feedback', () => {
  it('keeps loading and read failures in the connection disclosure and retries the read', async () => {
    const onRetryRead = vi.fn();
    const { update } = renderSidebar({ readLoading: true, onRetryRead });
    expect(screen.queryByRole('dialog', { name: '连接详情' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: /^连接状态/ }));
    expect(within(screen.getByRole('dialog', { name: '连接详情' })).getByText('正在读取工作区…')).toBeTruthy();
    await userEvent.keyboard('{Escape}');
    update({ readLoading: false, readError: 'areas down', onRetryRead });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('dialog', { name: '连接详情' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: '连接状态：连接异常' }));
    expect(within(screen.getByRole('dialog', { name: '连接详情' })).getByText('areas down')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: '重试读取' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });

  it('collects read and activity errors behind one indicator even with a collapsed sidebar', async () => {
    const onRetryRead = vi.fn();
    renderSidebar({ collapsed: true, readError: 'Areas are unavailable.', activityError: 'Track activity is unavailable.', onRetryRead });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.queryByRole('dialog', { name: '连接详情' })).toBeNull();
    expect(screen.getAllByRole('button', { name: /^连接状态/ })).toHaveLength(1);
    await userEvent.click(screen.getByRole('button', { name: /^连接状态/ }));
    expect(within(screen.getByRole('dialog', { name: '连接详情' })).getByText('Areas are unavailable.')).toBeTruthy();
    expect(within(screen.getByRole('dialog', { name: '连接详情' })).getByText('Track activity is unavailable.')).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: '重试读取' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });
});

describe('area disclosure', () => {
  it('collapses and re-expands an area track list from its group row', async () => {
    renderSidebar({ tracks: [track({ title: 'Inside' })] });
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Expand area Work' }));
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();
  });

  it('keeps an area collapsed when opening a track in it', async () => {
    const tracks = [track({ id: 'w9', title: 'Inside' })];
    const { update } = renderSidebar({ tracks });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();

    update({ tracks, currentPath: '/track/w9' });
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();
  });

  it('keeps an area collapsed when moving between Tracks in it', async () => {
    const tracks = [track({ id: 'w1', title: 'First' }), track({ id: 'w2', title: 'Second' })];
    const { update } = renderSidebar({ tracks, currentPath: '/track/w1' });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(screen.queryByRole('button', { name: /^Track Second/ })).toBeNull();

    update({ tracks, currentPath: '/track/w2' });
    expect(screen.queryByRole('button', { name: /^Track Second/ })).toBeNull();
  });
});

describe('area row', () => {
  it('carries the name and nothing else — no count, no identity dot', () => {
    const tracks = [
      track({ id: 'a' }),
      track({ id: 'b', closedAt: 5 }),
      track({ id: 'c' }),
    ];
    renderSidebar({ tracks, tracksByArea: new Map([['c1', tracks]]) });
    const name = screen.getByTitle('Work');
    const row = screen.getByRole('button', { name: 'Collapse area Work' });
    expect(name.textContent).toBe('Work');
    expect(name.closest('button')).toBe(row);
    expect(name.querySelectorAll('[style]').length).toBe(0);
  });

  it('makes the whole Area row a disclosure and never a navigation target', async () => {
    const onGo = vi.fn();
    renderSidebar({ tracks: [track()], onGo });
    const disclosure = screen.getByRole('button', { name: 'Collapse area Work' });
    expect(disclosure.getAttribute('aria-expanded')).toBe('true');
    expect(disclosure.querySelector('svg')).toBeTruthy();
    await userEvent.click(disclosure);
    expect(onGo).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: /^Track Task/ })).toBeNull();
  });

  it('toggles immediately for Enter and Space keyboard activation', async () => {
    renderSidebar({ tracks: [track()] });
    const disclosure = screen.getByRole('button', { name: 'Collapse area Work' });
    disclosure.focus();
    await userEvent.keyboard('{Enter}');
    expect(screen.getByRole('button', { name: 'Expand area Work' })).toBeTruthy();
    await userEvent.keyboard(' ');
    expect(screen.getByRole('button', { name: 'Collapse area Work' })).toBeTruthy();
  });

  /* The rail does not own the new-track surface; the group reports which Area the route belongs to. */
  it('starts a track in its own area from the row, without navigating into it', async () => {
    const onNewTrack = vi.fn();
    const onGo = vi.fn();
    renderSidebar({
      areas: [area(), area({ id: 'c2', name: 'Reading', sort: 2 })],
      tracksByArea: new Map([['c1', []], ['c2', []]]),
      onNewTrack,
      onGo,
    });
    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    expect(onNewTrack.mock.calls).toEqual([['c2']]);
    expect(onGo).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Collapse area Reading' })).toBeTruthy();
  });
});

describe('new area', () => {
  it('requests the shared editor from both the header and zero-state action', async () => {
    const onRequestCreateArea = vi.fn();
    const view = renderSidebar({ onRequestCreateArea });
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    expect(onRequestCreateArea).toHaveBeenCalledTimes(1);

    view.update({ areas: [], tracksByArea: new Map() });
    await userEvent.click(screen.getByRole('button', { name: 'Create your first area' }));
    expect(onRequestCreateArea).toHaveBeenCalledTimes(2);
  });
});

describe('edit area', () => {
  it('does not overload the disclosure double-click with editing', async () => {
    const onRequestEditArea = vi.fn();
    const onGo = vi.fn();
    renderSidebar({ tracks: [track({ title: 'Inside' })], onRequestEditArea, onGo });
    await userEvent.dblClick(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(onRequestEditArea).not.toHaveBeenCalled();
    expect(screen.getByRole('button', { name: 'Collapse area Work' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();
    expect(onGo).not.toHaveBeenCalled();
  });

  it('is discoverable from the Area actions menu', async () => {
    const onRequestEditArea = vi.fn();
    renderSidebar({ onRequestEditArea });
    await userEvent.click(screen.getByRole('button', { name: 'Area actions for Work' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Edit area' }));
    expect(onRequestEditArea).toHaveBeenCalledWith(area());
  });
});

describe('destructive confirms', () => {
  it('deletes a track only after Confirm, and nothing on Cancel', async () => {
    const onDeleteTrack = vi.fn();
    renderSidebar({ tracks: [track({ id: 'w1', title: 'Task' })], onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    expect(screen.getByRole('dialog', { name: 'Delete this track?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onDeleteTrack).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    expect(onDeleteTrack).toHaveBeenCalledWith('w1', expect.any(AbortSignal));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('deletes an area only after Confirm', async () => {
    const onDeleteArea = vi.fn();
    renderSidebar({ onDeleteArea });

    await requestAreaDelete();
    // Deleting an area cascades to every track inside it, so it is the one operation
    // behind a typed confirm.
    expect(screen.getByRole('dialog', { name: 'Delete Work?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(onDeleteArea).not.toHaveBeenCalled();

    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(onDeleteArea).toHaveBeenCalledWith('c1', expect.any(AbortSignal));
  });

  it('states that the cascade count is unknown when the area track query has no data', async () => {
    renderSidebar({ tracksByArea: new Map() });
    await requestAreaDelete();
    expect(screen.getByRole('dialog').textContent).toContain('The number of tracks is not available.');
    expect(screen.getByRole('dialog').textContent).not.toContain('deletes 0 tracks');
  });

  it('describes deletion of a genuinely empty area without claiming it deletes zero tracks', async () => {
    renderSidebar({ tracksByArea: new Map([['c1', []]]) });
    await requestAreaDelete();
    expect(screen.getByRole('dialog').textContent).toContain('This deletes the area.');
    expect(screen.getByRole('dialog').textContent).not.toContain('deletes 0 tracks');
  });

  it('keeps the confirm mounted while the delete is in flight and clears it on rejection', async () => {
    let reject: (reason: Error) => void = () => {};
    let signal: AbortSignal | undefined;
    const onDeleteTrack = vi.fn((_id: string, requestSignal: AbortSignal) => {
      signal = requestSignal;
      return new Promise<void>((_resolve, rejectFn) => { reject = rejectFn; });
    });
    renderSidebar({ tracks: [track({ id: 'w1', title: 'Task' })], onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    // Busy, not `disabled`: focus is on Confirm at this instant and `disabled`
    // would drop it out of the trap.
    const confirm = screen.getByRole('button', { name: 'Deleting…' });
    expect(confirm.hasAttribute('disabled')).toBe(false);
    expect(confirm.getAttribute('aria-disabled')).toBe('true');
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(cancel.hasAttribute('disabled')).toBe(false);
    expect(screen.getByRole('dialog').textContent).toContain('Closing this dialog cancels the delete request.');
    await userEvent.click(cancel);
    expect(screen.queryByRole('dialog', { name: 'Delete this track?' })).toBeNull();
    expect(signal?.aborted).toBe(true);

    reject(new DOMException('aborted', 'AbortError'));
    await screen.findByRole('button', { name: 'Delete Task' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('can delete a second target immediately after canceling the first request', async () => {
    const deleted: string[] = [];
    const onDeleteTrack = vi.fn((id: string, signal: AbortSignal) => {
      deleted.push(id);
      if (id !== 'w1') return Promise.resolve();
      return new Promise<void>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    });
    renderSidebar({ tracks: [track({ id: 'w1', title: 'Alpha' }), track({ id: 'w2', title: 'Beta' })], onDeleteTrack });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Alpha' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete Beta' }));
    expect(screen.getByRole('button', { name: 'Delete track' }).getAttribute('aria-busy')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Delete track' }));
    expect(deleted).toEqual(['w1', 'w2']);
  });
});

describe('pin', () => {
  it('pins straight from the row without a confirm', async () => {
    const onSetPinned = vi.fn();
    renderSidebar({ tracks: [track({ id: 'w1', title: 'Task' })], onSetPinned });
    await userEvent.click(screen.getByRole('button', { name: 'Pin Task' }));
    expect(onSetPinned.mock.calls).toEqual([['w1', true]]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('user menu', () => {
  it('opens Settings, Plugins and Sign out from the avatar, and calls the injected callbacks', async () => {
    const onOpenSettings = vi.fn();
    const onOpenPlugins = vi.fn();
    const onSignOut = vi.fn();
    renderSidebar({ onOpenSettings, onOpenPlugins, onSignOut, userLabel: 'Kenji Xie' });

    const avatar = screen.getByRole('button', { name: 'Account menu for Kenji Xie' });
    expect(avatar.textContent).toBe('KX');
    await userEvent.click(avatar);
    expect(screen.getAllByRole('menuitem').map((node) => node.textContent))
      .toEqual(['Settings', 'Plugins', 'Sign out']);

    await userEvent.click(screen.getByRole('menuitem', { name: 'Settings' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole('button', { name: 'Account menu for Kenji Xie' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Plugins' }));
    expect(onOpenPlugins).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole('button', { name: 'Account menu for Kenji Xie' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Sign out' }));
    expect(onSignOut).toHaveBeenCalledTimes(1);
  });
});

describe('collapse toggle', () => {
  it('pairs the mark with a clear Today destination in the expanded rail', async () => {
    const onGo = vi.fn();
    renderSidebar({ onGo });
    const today = screen.getByRole('button', { name: 'Go to Today' });
    expect(today.textContent).toBe('Today');
    expect(today.querySelector('[aria-hidden="true"]')).toBeTruthy();
    await userEvent.click(today);
    expect(onGo).toHaveBeenCalledWith({ name: 'today' });
  });

  /* `AppShell` owns `collapsed`, because collapsing changes the shell grid column;
   * the click reports upward and the collapsed rendering is driven by the prop. */
  it('reports the toggle upward instead of collapsing itself', async () => {
    const onToggleCollapsed = vi.fn();
    renderSidebar({ tracks: [track({ title: 'Inside' })], onToggleCollapsed });
    const toggle = screen.getByRole('button', { name: 'Collapse sidebar' });
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    await userEvent.click(toggle);
    expect(onToggleCollapsed).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
  });

  it('drops to an icon strip when told it is collapsed, and stays navigable', () => {
    const { update } = renderSidebar({ tracks: [track({ title: 'Inside' })] });
    update({ collapsed: true });

    // No section labels: 11px uppercase does not fit in 44px.
    expect(screen.queryAllByRole('heading')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();
    // The area is still reachable, named for assistive tech and initialled for
    // sighted users; this surface stays greyscale.
    const item = screen.getByRole('button', { name: 'Show area Work' });
    expect(item.textContent).toBe('W');
    expect(screen.getByRole('button', { name: 'Account menu for You' })).toBeTruthy();
    const expand = screen.getByRole('button', { name: 'Expand sidebar' });
    expect(expand.getAttribute('aria-expanded')).toBe('false');
    expect(expand.querySelector('[aria-hidden="true"]')).toBeTruthy();

    update({ collapsed: false });
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
  });

  it('uses a collapsed Area initial to reveal its group, never to navigate', async () => {
    const onToggleCollapsed = vi.fn();
    const onGo = vi.fn();
    renderSidebar({ collapsed: true, onToggleCollapsed, onGo });
    await userEvent.click(screen.getByRole('button', { name: 'Show area Work' }));
    expect(onToggleCollapsed).toHaveBeenCalledTimes(1);
    expect(onGo).not.toHaveBeenCalled();
  });

  it('leaves focus and the final scroll on the chosen Area, not the active Track elsewhere', async () => {
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
    const active = track({ id: 'w1', areaId: 'c1', title: 'Active' });
    const reading = area({ id: 'c2', name: 'Reading', sort: 2 });
    const { update } = renderSidebar({
      collapsed: true,
      currentPath: '/track/w1',
      areas: [area(), reading],
      tracks: [active],
      tracksByArea: new Map([['c1', [active]], ['c2', []]]),
    });
    await userEvent.click(screen.getByRole('button', { name: 'Show area Reading' }));
    update({ collapsed: false });

    const disclosure = screen.getByRole('button', { name: 'Collapse area Reading' });
    await waitFor(() => expect(document.activeElement).toBe(disclosure));
    await waitFor(() => expect(scrolled.at(-1)).toBe(disclosure));
    expect(scrolled.some((element) => element.getAttribute('aria-current') === 'page')).toBe(true);
    expect(scrollIntoView).toHaveBeenLastCalledWith({ block: 'nearest' });
    if (originalScrollIntoView === undefined) {
      Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
    } else {
      Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', originalScrollIntoView);
    }
  });

  it('shows the waiting count as the strip\'s only figure, with no dot beside it', () => {
    const { update } = renderSidebar({
      // The count is the kernel's verdict (input or failed), not the open/closed state:
      // a closed track the kernel has said nothing about is not waiting on anyone.
      tracks: [
        track({ id: 'a', closedAt: 5 }),
        track({ id: 'b', attention: 'input' }),
        track({ id: 'c', closedAt: 5, attention: 'failed' }),
      ],
    });
    update({ collapsed: true });
    const count = screen.getByLabelText('2 waiting on you');
    expect(count.textContent).toBe('2');
  });
});


it('reads the track receipt against the activity high-water mark, not updatedAt', () => {
  /* `updatedAt` moves on a rename or a pin; the receipt compares the kernel's
   * completion high-water mark, and `activityAt: null` is never unread. The scope
   * is entered at server time 100 (the baseline). */
  const preferences = createUiPreferences(memoryStorage());
  preferences.setReadScope('db1', 100);
  renderSidebar({
    tracks: [
      track({ id: 'fresh', title: 'Fresh', updatedAt: 1, activityAt: 150 }),
      track({ id: 'seen', title: 'Seen', updatedAt: 1_000, activityAt: 50 }),
      track({ id: 'never', title: 'Never', updatedAt: 1_000, activityAt: null }),
    ],
  }, preferences);
  const marker = (title: string) => screen.getByRole('button', { name: new RegExp(`^Track ${title}`) })
    .parentElement?.querySelector('[data-nc-activity]')?.getAttribute('data-nc-activity') ?? null;
  expect(marker('Fresh')).toBe('unread');
  expect(marker('Seen')).toBeNull();
  expect(marker('Never')).toBeNull();
});

it('restores Area disclosure after the shell is remounted', async () => {
  const storage = memoryStorage();
  const tracks = [track({ title: 'Inside' })];
  const first = renderSidebar({ tracks }, createUiPreferences(storage));
  await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
  first.unmount();
  renderSidebar({ tracks, currentPath: '/track/w1' }, createUiPreferences(storage));
  expect(screen.getByRole('button', { name: 'Expand area Work' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();
});


describe('shared sidebar groups', () => {
  const headings = () => screen.getAllByRole('heading').map((node) => node.textContent);
  async function openOptions() {
    await userEvent.click(screen.getByRole('button', { name: 'Sidebar view options' }));
  }

  it('toggles Unread and Running independently and remembers both choices', async () => {
    const storage = memoryStorage();
    const preferences = createUiPreferences(storage);
    preferences.setReadScope('db', 1);
    const props = { tracks: [track({ activityAt: 10, working: true, attention: 'input', pinnedAt: 5 })] };
    renderSidebar(props, preferences);
    expect(headings()).toEqual(['Waiting on you', 'Pinned', 'Areas']);
    await openOptions();
    expect(screen.queryByRole('menuitemcheckbox')).toBeNull();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Hidden groups' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Show Unread' }));
    expect(screen.queryByRole('menu')).toBeNull();
    expect(headings()).toEqual(['Waiting on you', 'Pinned', 'Unread', 'Areas']);
    await openOptions();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Hidden groups' }));
    expect(screen.queryByRole('menuitem', { name: 'Show Unread' })).toBeNull();
    await userEvent.click(screen.getByRole('menuitem', { name: 'Show Running' }));
    expect(headings()).toEqual(['Waiting on you', 'Pinned', 'Unread', 'Running', 'Areas']);
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Sidebar view options' }));
    cleanup();
    const restored = createUiPreferences(storage);
    restored.setReadScope('db', 1);
    renderSidebar(props, restored);
    expect(headings()).toEqual(['Waiting on you', 'Pinned', 'Unread', 'Running', 'Areas']);
    await userEvent.click(screen.getByRole('button', { name: 'Group actions for Unread' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Hide group' }));
    expect(headings()).toEqual(['Waiting on you', 'Pinned', 'Running', 'Areas']);
  });

  it('uses completion receipts for Unread and live working state for Running independently of attention', () => {
    const preferences = createUiPreferences();
    preferences.setReadScope('db', 1);
    preferences.setSidebarGroupVisible('unread', true);
    preferences.setSidebarGroupVisible('running', true);
    const tracks = [
      track({ id: 'both', title: 'Both', activityAt: 10, working: true, attention: 'input', closedAt: 5 }),
      track({ id: 'read', title: 'Read', activityAt: 8, updatedAt: 99 }),
      track({ id: 'quiet', title: 'Renamed', activityAt: null, updatedAt: 100 }),
    ];
    preferences.markRead('track', 'read', 8);
    const { update } = renderSidebar({ tracks }, preferences);
    const unreadGroup = screen.getByRole('group', { name: 'Unread' });
    const runningGroup = screen.getByRole('group', { name: 'Running' });
    expect(within(unreadGroup).getAllByRole('button', { name: /^Track / })).toHaveLength(1);
    expect(within(runningGroup).getAllByRole('button', { name: /^Track / })).toHaveLength(1);
    act(() => preferences.markRead('track', 'both', 10));
    expect(headings()).toEqual(['Waiting on you', 'Unread', 'Running', 'Areas']);
    expect(within(unreadGroup).queryAllByRole('button', { name: /^Track / })).toHaveLength(0);
    update({ tracks: tracks.map((row) => ({ ...row, working: false })) });
    expect(headings()).toEqual(['Waiting on you', 'Unread', 'Running', 'Areas']);
    expect(within(runningGroup).queryAllByRole('button', { name: /^Track / })).toHaveLength(0);
    update({ tracks: tracks.map((row) => row.id === 'both' ? { ...row, working: false, activityAt: 11 } : row) });
    expect(headings()).toEqual(['Waiting on you', 'Unread', 'Running', 'Areas']);
    expect(within(unreadGroup).getAllByRole('button', { name: /^Track / })).toHaveLength(1);
  });

  it('shares disclosure and the five-row reveal across shortcut and Area groups', async () => {
    const preferences = createUiPreferences(memoryStorage());
    const tracks = Array.from({ length: 7 }, (_, index) => track({ id: `p${index}`, title: `P${index}`, pinnedAt: 20 - index }));
    const { update } = renderSidebar({ tracks, currentPath: '/track/p6' }, preferences);
    const group = screen.getByRole('group', { name: 'Pinned' });
    expect(within(group).getAllByRole('button', { name: /^Track / })).toHaveLength(6);
    await userEvent.click(within(group).getByRole('button', { name: 'Show 1 more in Pinned' }));
    expect(within(group).getAllByRole('button', { name: /^Track / })).toHaveLength(7);
    await userEvent.click(screen.getByRole('button', { name: 'Collapse Pinned' }));
    expect(within(group).queryAllByRole('button', { name: /^Track / })).toHaveLength(0);
    update({ tracks, currentPath: '/track/p0' });
    expect(screen.getByRole('button', { name: 'Expand Pinned' }).getAttribute('aria-expanded')).toBe('false');
    await userEvent.click(screen.getByRole('button', { name: 'Expand Pinned' }));
    expect(within(group).getAllByRole('button', { name: /^Track / })).toHaveLength(7);
    await userEvent.click(screen.getByRole('button', { name: 'Collapse Areas' }));
    expect(screen.queryByRole('button', { name: 'Collapse area Work' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Expand Areas' }));
    expect(screen.getByRole('button', { name: 'Collapse area Work' })).toBeTruthy();
  });

  it('never surfaces system Tracks in enabled shortcut groups', () => {
    const preferences = createUiPreferences();
    preferences.setReadScope('db', 1);
    preferences.setSidebarGroupVisible('unread', true);
    preferences.setSidebarGroupVisible('running', true);
    renderSidebar({ areas: [area({ kind: 'system' })], tracks: [track({ activityAt: 10, working: true })] }, preferences);
    expect(headings()).toEqual(['Unread', 'Running', 'Areas']);
    expect(screen.queryByRole('button', { name: /^Track / })).toBeNull();
  });
});


describe('sidebar group management', () => {
  const headings = () => screen.getAllByRole('heading').map((node) => node.textContent);
  async function action(menu: string, item: string) {
    await userEvent.click(screen.getByRole('button', { name: menu }));
    await userEvent.click(screen.getByRole('menuitem', { name: item }));
  }
  async function restore(title: string, isArea = false) {
    await userEvent.click(screen.getByRole('button', { name: 'Sidebar view options' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Hidden groups' }));
    await userEvent.click(screen.getByRole('menuitem', { name: `Show ${isArea ? 'area ' : ''}${title}` }));
  }

  it('moves visible top-level siblings, disables boundaries and restores hidden groups in their saved slots', async () => {
    const preferences = createUiPreferences();
    preferences.setSidebarGroupVisible('unread', true);
    preferences.setSidebarGroupVisible('running', true);
    preferences.setReadScope('db', 1);
    renderSidebar({ tracks: [track({ pinnedAt: 5, attention: 'input', working: true, activityAt: 10 })] }, preferences);
    await userEvent.click(screen.getByRole('button', { name: 'Group actions for Waiting on you' }));
    expect(screen.getByRole('menuitem', { name: 'Move up' }).getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(screen.getByRole('menuitem', { name: 'Move down' }));
    expect(headings()).toEqual(['Pinned', 'Waiting on you', 'Unread', 'Running', 'Areas']);
    await action('Group actions for Waiting on you', 'Hide group');
    expect(headings()).toEqual(['Pinned', 'Unread', 'Running', 'Areas']);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'Sidebar view options' }));
    await restore('Waiting on you');
    expect(headings()).toEqual(['Pinned', 'Waiting on you', 'Unread', 'Running', 'Areas']);
    await userEvent.click(screen.getByRole('button', { name: 'Group actions for Areas' }));
    expect(screen.getByRole('menuitem', { name: 'Move down' }).getAttribute('aria-disabled')).toBe('true');
    await userEvent.keyboard('{Escape}');
  });

  it('keeps Area movement personal, restores a hidden Area even when its parent is hidden, and remembers layout', async () => {
    const storage = memoryStorage();
    const preferences = createUiPreferences(storage);
    const areas = [area(), area({ id: 'c2', name: 'Reading', sort: 2 })];
    const props = { areas, tracksByArea: new Map([['c1', []], ['c2', []]]) };
    const view = renderSidebar(props, preferences);
    await action('Area actions for Work', 'Move down');
    expect(screen.getAllByRole('button', { name: /^Collapse area / }).map((node) => node.textContent)).toEqual(['Reading', 'Work']);
    expect(areas.map((row) => [row.id, row.sort])).toEqual([['c1', 1], ['c2', 2]]);
    await action('Area actions for Work', 'Hide group');
    expect(screen.queryByRole('button', { name: 'Collapse area Work' })).toBeNull();
    await action('Group actions for Areas', 'Hide group');
    expect(screen.queryByRole('heading', { name: 'Areas' })).toBeNull();
    await restore('Work', true);
    expect(screen.getAllByRole('button', { name: /^Collapse area / }).map((node) => node.textContent)).toEqual(['Reading', 'Work']);
    await action('Area actions for Reading', 'Hide group');
    view.unmount();
    renderSidebar(props, createUiPreferences(storage));
    expect(screen.queryByRole('button', { name: 'Collapse area Reading' })).toBeNull();
    await restore('Reading', true);
    expect(screen.getAllByRole('button', { name: /^Collapse area / }).map((node) => node.textContent)).toEqual(['Reading', 'Work']);
  });

  it('appends new Areas, forgets deleted ones and never restores a system Area from saved preferences', () => {
    const preferences = createUiPreferences();
    preferences.setSidebarOrder('areas', ['deleted', 'sys', 'c2', 'c2', 'c1']);
    preferences.setSidebarGroupVisible('area:sys', false);
    const areas = [area(), area({ id: 'c2', name: 'Reading' }), area({ id: 'new', name: 'New' }), area({ id: 'sys', name: 'System', kind: 'system' })];
    renderSidebar({ areas }, preferences);
    expect(screen.getAllByRole('button', { name: /^Collapse area / }).map((node) => node.textContent)).toEqual(['Reading', 'Work', 'New']);
    expect(screen.queryByRole('button', { name: 'Area actions for System' })).toBeNull();
  });

  it('hiding the current Area changes only sidebar display and applies to the collapsed strip', async () => {
    const preferences = createUiPreferences();
    const onGo = vi.fn();
    const props = { tracks: [track({ title: 'Current', working: true })], currentPath: '/track/w1', onGo };
    const { update } = renderSidebar(props, preferences);
    await action('Area actions for Work', 'Hide group');
    expect(onGo).not.toHaveBeenCalled();
    update({ ...props, collapsed: true });
    expect(screen.queryByRole('button', { name: 'Show area Work' })).toBeNull();
  });
});


it('distinguishes hidden Areas from workspace groups with the same title', async () => {
  renderSidebar({ areas: [area({ name: 'Unread' })] });
  await userEvent.click(screen.getByRole('button', { name: 'Area actions for Unread' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Hide group' }));
  await userEvent.click(screen.getByRole('button', { name: 'Sidebar view options' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Hidden groups' }));
  expect(screen.getByRole('menuitem', { name: 'Show Unread' })).toBeTruthy();
  expect(screen.getByRole('menuitem', { name: 'Show area Unread' })).toBeTruthy();
});


it('keeps Show recovery for every built-in group after all groups are hidden', async () => {
  const preferences = createUiPreferences();
  preferences.setReadScope('db', 1);
  preferences.setSidebarGroupVisible('unread', true);
  preferences.setSidebarGroupVisible('running', true);
  const onGo = vi.fn();
  const rows = [track({ title: 'Recoverable', pinnedAt: 5, attention: 'input', working: true, activityAt: 10 })];
  renderSidebar({ tracks: rows, onGo }, preferences);
  const titles = ['Waiting on you', 'Pinned', 'Unread', 'Running', 'Areas'];
  for (const title of titles) {
    await userEvent.click(screen.getByRole('button', { name: `Group actions for ${title}` }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Hide group' }));
  }
  expect(screen.queryAllByRole('heading')).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Sidebar view options' }));
  await userEvent.keyboard('{ArrowDown}{ArrowRight}');
  await screen.findByRole('menuitem', { name: 'Show Waiting on you' });
  for (const title of titles) expect(screen.getByRole('menuitem', { name: `Show ${title}` })).toBeTruthy();
  await userEvent.keyboard('{Escape}{Escape}');
  for (const title of titles) {
    await userEvent.click(screen.getByRole('button', { name: 'Sidebar view options' }));
    await userEvent.keyboard('{ArrowDown}{ArrowRight}');
    await userEvent.click(await screen.findByRole('menuitem', { name: `Show ${title}` }));
  }
  expect(screen.getAllByRole('heading').map(node => node.textContent)).toEqual(titles);
  expect(screen.getAllByRole('button', { name: /^Track Recoverable/ })).toHaveLength(5);
  expect(rows[0]?.pinnedAt).toBe(5);
  expect(onGo).not.toHaveBeenCalled();
});
