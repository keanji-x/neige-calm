// @vitest-environment jsdom
// Behaviour of the workspace rail: disclosure, badges, create/delete, menu.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Area } from '../../../../core/domain/area.ts';
import { NEUTRAL_ACTIVITY, type Track } from '../../../../core/domain/track.ts';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

function area(overrides: Partial<Area> = {}): Area {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Task', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

type Props = Parameters<typeof Sidebar>[0];

function renderSidebar(props: Partial<Props> = {}) {
  const build = (overrides: Partial<Props>) => {
    const merged = { ...props, ...overrides };
    const tracks = merged.tracks ?? [];
    return (
      <ThemeProvider storage={memoryStorage()}>
        <Sidebar
          areas={merged.areas ?? [area()]}
          tracksByArea={merged.tracksByArea ?? new Map([['c1', tracks]])}
          tracks={tracks}
          currentPath={merged.currentPath ?? '/'}
          onGo={merged.onGo ?? vi.fn()}
          onCreateArea={merged.onCreateArea ?? vi.fn()}
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
      </ThemeProvider>
    );
  };
  const result = render(build({}));
  return { ...result, update: (overrides: Partial<Props>) => result.rerender(build(overrides)) };
}

describe('workspace read feedback', () => {
  it('shows loading, read failure, and retries the workspace read', async () => {
    const onRetryRead = vi.fn();
    const { update } = renderSidebar({ readLoading: true, onRetryRead });
    expect(screen.getByRole('status').textContent).toContain('Loading workspace');
    update({ readLoading: false, readError: 'areas down', onRetryRead });
    expect(screen.getByRole('alert').textContent).toContain('areas down');
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });

  it('warns that track activity is unavailable and retries it', async () => {
    const onRetryRead = vi.fn();
    renderSidebar({ activityError: 'overlays down', onRetryRead });
    expect(screen.getByRole('alert').textContent).toContain('Track activity is unavailable: overlays down');
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });
});

describe('area disclosure', () => {
  it('collapses and re-expands an area track list from its chevron', async () => {
    renderSidebar({ tracks: [track({ title: 'Inside' })] });
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Expand area Work' }));
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();
  });

  it('re-expands the area holding the track the user just opened', async () => {
    const tracks = [track({ id: 'w9', title: 'Inside' })];
    const { update } = renderSidebar({ tracks });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse area Work' }));
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();

    update({ tracks, currentPath: '/track/w9' });
    expect(screen.getByRole('button', { name: /^Track Inside/ })).toBeTruthy();
  });
});

describe('area row', () => {
  it('excludes archived tracks from shortcuts and area groups', () => {
    const archived = track({ title: 'Filed away', lifecycle: 'blocked', archivedAt: 10, pinnedAt: 9 });
    renderSidebar({ tracks: [archived], tracksByArea: new Map([['c1', [archived]]]) });
    expect(screen.queryByRole('button', { name: /Filed away/ })).toBeNull();
  });

  /*
   * The count is gone, and this asserts its absence.
   *
   * It answered a question nobody asks: you come to the rail to find *a* track,
   * not to learn how many an area holds, and the number drove no decision here.
   * It spent a grid column and a tone saying so. What the rail owes you about a
   * area is already under it — its rows.
   */
  it('carries the name and nothing else — no count, no identity dot', () => {
    const tracks = [
      track({ id: 'a', lifecycle: 'blocked' }),
      track({ id: 'b', lifecycle: 'draft' }),
      track({ id: 'c', lifecycle: 'draft' }),
    ];
    renderSidebar({ tracks, tracksByArea: new Map([['c1', tracks]]) });
    const row = screen.getByRole('button', { name: 'Work' });
    expect(row.textContent).toBe('Work');
    expect(row.querySelectorAll('[style]').length).toBe(0);
  });

  // The disclosure control is a *sibling* of the row, not a child: a button
  // inside a button is invalid HTML and trips axe's `nested-interactive`.
  it('exposes disclosure as its own control outside the navigation button', () => {
    renderSidebar({ tracks: [track()] });
    const row = screen.getByRole('button', { name: 'Work' });
    const chevron = screen.getByRole('button', { name: 'Collapse area Work' });
    expect(row.contains(chevron)).toBe(false);
    expect(chevron.getAttribute('aria-expanded')).toBe('true');
  });

  /* The rail does not own the new-track surface — since #1211 it is a route,
     and the rail only navigates. `AppShell` owns the seam, because the
     area page's `+` opens the same one. All the row reports is which area. */
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
  });
});

describe('new area', () => {
  it('submits on Enter with a palette colour, and cancels on Escape', async () => {
    const onCreateArea = vi.fn();
    renderSidebar({ onCreateArea });

    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Area name' }), 'Reading{Escape}');
    expect(onCreateArea).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Area name' }), 'Reading{Enter}');
    expect(onCreateArea).toHaveBeenCalledTimes(1);
    const [name, color] = onCreateArea.mock.calls[0] as [string, string];
    expect(name).toBe('Reading');
    expect(AREA_PALETTE).toContain(color);
  });

  it('submits on blur', async () => {
    const onCreateArea = vi.fn();
    renderSidebar({ onCreateArea });
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Area name' }), 'Later');
    await userEvent.click(screen.getByRole('button', { name: 'Work' }));
    expect(onCreateArea.mock.calls.map((call) => (call as string[])[0])).toEqual(['Later']);
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

    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    // §6.13 / CR-5a — the title names the area, and Confirm stays blocked until
    // the name is reproduced. Deleting an area cascades to every track inside it;
    // it is the one operation in the product that earns a typed confirm, and
    // this rail entry shares that dialog with the area page's header button.
    expect(screen.getByRole('dialog', { name: 'Delete Work?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(onDeleteArea).not.toHaveBeenCalled();

    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete area' }));
    expect(onDeleteArea).toHaveBeenCalledWith('c1', expect.any(AbortSignal));
  });

  it('states that the cascade count is unknown when the area track query has no data', async () => {
    renderSidebar({ tracksByArea: new Map() });
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
    expect(screen.getByRole('dialog').textContent).toContain('The number of tracks is not available.');
    expect(screen.getByRole('dialog').textContent).not.toContain('deletes 0 tracks');
  });

  it('describes deletion of a genuinely empty area without claiming it deletes zero tracks', async () => {
    renderSidebar({ tracksByArea: new Map([['c1', []]]) });
    await userEvent.click(screen.getByRole('button', { name: 'Delete area Work' }));
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
    // CR-6 — busy, not `disabled`. Cancel stays a real exit, the dialog stays
    // mounted for the whole await, and Confirm stays focusable: focus is on it
    // at this instant and `disabled` would drop it out of the trap.
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
    // Two destinations and the way out. The theme cycler is deliberately gone:
    // it was the one item that acted instead of navigating, and Settings ›
    // General states the same preference where its effect is visible.
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
  it('places the mark with the wordmark in the expanded rail', () => {
    renderSidebar();
    const brand = screen.getByRole('button', { name: 'neige · calm' });
    expect(brand.querySelector('[aria-hidden="true"]')).toBeTruthy();
  });

  /*
   * The rail does not own `collapsed` — `AppShell` does, because collapsing
   * changes the *shell grid column*, not just what the rail draws. A version of
   * this suite that clicked the toggle and expected the rail to change was
   * asserting against a `vi.fn()`; it passed only while the state still lived
   * here. So: the click reports upward, and the collapsed rendering is driven
   * by the prop.
   */
  it('reports the toggle upward instead of collapsing itself', async () => {
    const onToggleCollapsed = vi.fn();
    renderSidebar({ tracks: [track({ title: 'Inside' })], onToggleCollapsed });
    const toggle = screen.getByRole('button', { name: 'Collapse sidebar' });
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    await userEvent.click(toggle);
    expect(onToggleCollapsed).toHaveBeenCalledTimes(1);
    // Nothing changed here, because nothing here owns it.
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
  });

  it('drops to an icon strip when told it is collapsed, and stays navigable', () => {
    const { update } = renderSidebar({ tracks: [track({ title: 'Inside' })] });
    update({ collapsed: true });

    // No section labels: 11px uppercase does not fit in 44px, and the strip
    // answers "where am I", not "what is there".
    expect(screen.queryAllByRole('heading')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /^Track Inside/ })).toBeNull();
    // The area is still reachable, named for assistive tech and initialled for
    // sighted users — a letter, not one of eight area hues, because §7.5 keeps
    // this surface greyscale apart from the current location and "waiting".
    const item = screen.getByRole('button', { name: 'Work' });
    expect(item.textContent).toBe('W');
    expect(screen.getByRole('button', { name: 'Account menu for You' })).toBeTruthy();
    const expand = screen.getByRole('button', { name: 'Expand sidebar' });
    expect(expand.getAttribute('aria-expanded')).toBe('false');
    expect(expand.querySelector('[aria-hidden="true"]')).toBeTruthy();

    update({ collapsed: false });
    expect(screen.getByRole('heading', { name: 'Areas' })).toBeTruthy();
  });

  it('shows the waiting count as the strip\'s only figure, with no dot beside it', () => {
    const { update } = renderSidebar({
      tracks: [track({ id: 'a', lifecycle: 'blocked' }), track({ id: 'b', lifecycle: 'draft' })],
    });
    update({ collapsed: true });
    const count = screen.getByLabelText('1 waiting on you');
    expect(count.textContent).toBe('1');
  });
});
