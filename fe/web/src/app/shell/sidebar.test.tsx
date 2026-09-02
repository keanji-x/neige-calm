// @vitest-environment jsdom
// Behaviour of the workspace rail: disclosure, badges, create/delete, menu.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove } from '../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type Wave } from '../../../../core/domain/wave.ts';
import { COVE_PALETTE } from '../../features/cove/palette.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { Sidebar } from './sidebar.tsx';

afterEach(() => { cleanup(); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}

function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Task', sort: 1, lifecycle: 'draft', cwd: '/tmp',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

type Props = Parameters<typeof Sidebar>[0];

function renderSidebar(props: Partial<Props> = {}) {
  const build = (overrides: Partial<Props>) => {
    const merged = { ...props, ...overrides };
    const waves = merged.waves ?? [];
    return (
      <ThemeProvider storage={memoryStorage()}>
        <Sidebar
          coves={merged.coves ?? [cove()]}
          wavesByCove={merged.wavesByCove ?? new Map([['c1', waves]])}
          waves={waves}
          currentPath={merged.currentPath ?? '/'}
          onGo={merged.onGo ?? vi.fn()}
          onCreateCove={merged.onCreateCove ?? vi.fn()}
          onDeleteCove={merged.onDeleteCove ?? vi.fn()}
          onNewWave={merged.onNewWave ?? vi.fn()}
          onSetPinned={merged.onSetPinned ?? vi.fn()}
          onDeleteWave={merged.onDeleteWave ?? vi.fn()}
          collapsed={merged.collapsed ?? false}
          onToggleCollapsed={merged.onToggleCollapsed ?? vi.fn()}
          onOpenSettings={merged.onOpenSettings ?? vi.fn()}
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
    update({ readLoading: false, readError: 'coves down', onRetryRead });
    expect(screen.getByRole('alert').textContent).toContain('coves down');
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });

  it('warns that wave activity is unavailable and retries it', async () => {
    const onRetryRead = vi.fn();
    renderSidebar({ activityError: 'overlays down', onRetryRead });
    expect(screen.getByRole('alert').textContent).toContain('Wave activity is unavailable: overlays down');
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryRead).toHaveBeenCalledTimes(1);
  });
});

describe('cove disclosure', () => {
  it('collapses and re-expands a cove wave list from its chevron', async () => {
    renderSidebar({ waves: [wave({ title: 'Inside' })] });
    expect(screen.getByRole('button', { name: /^Wave Inside/ })).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'Collapse cove Work' }));
    expect(screen.queryByRole('button', { name: /^Wave Inside/ })).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Expand cove Work' }));
    expect(screen.getByRole('button', { name: /^Wave Inside/ })).toBeTruthy();
  });

  it('re-expands the cove holding the wave the user just opened', async () => {
    const waves = [wave({ id: 'w9', title: 'Inside' })];
    const { update } = renderSidebar({ waves });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse cove Work' }));
    expect(screen.queryByRole('button', { name: /^Wave Inside/ })).toBeNull();

    update({ waves, currentPath: '/wave/w9' });
    expect(screen.getByRole('button', { name: /^Wave Inside/ })).toBeTruthy();
  });
});

describe('cove row', () => {
  it('excludes archived waves from shortcuts and cove groups', () => {
    const archived = wave({ title: 'Filed away', lifecycle: 'blocked', archivedAt: 10, pinnedAt: 9 });
    renderSidebar({ waves: [archived], wavesByCove: new Map([['c1', [archived]]]) });
    expect(screen.queryByRole('button', { name: /Filed away/ })).toBeNull();
  });

  /*
   * The count is gone, and this asserts its absence.
   *
   * It answered a question nobody asks: you come to the rail to find *a* wave,
   * not to learn how many a cove holds, and the number drove no decision here.
   * It spent a grid column and a tone saying so. What the rail owes you about a
   * cove is already under it — its rows.
   */
  it('carries the name and nothing else — no count, no identity dot', () => {
    const waves = [
      wave({ id: 'a', lifecycle: 'blocked' }),
      wave({ id: 'b', lifecycle: 'draft' }),
      wave({ id: 'c', lifecycle: 'draft' }),
    ];
    renderSidebar({ waves, wavesByCove: new Map([['c1', waves]]) });
    const row = screen.getByRole('button', { name: 'Work' });
    expect(row.textContent).toBe('Work');
    expect(row.querySelectorAll('[style]').length).toBe(0);
  });

  // The disclosure control is a *sibling* of the row, not a child: a button
  // inside a button is invalid HTML and trips axe's `nested-interactive`.
  it('exposes disclosure as its own control outside the navigation button', () => {
    renderSidebar({ waves: [wave()] });
    const row = screen.getByRole('button', { name: 'Work' });
    const chevron = screen.getByRole('button', { name: 'Collapse cove Work' });
    expect(row.contains(chevron)).toBe(false);
    expect(chevron.getAttribute('aria-expanded')).toBe('true');
  });

  /* The rail does not own the new-wave surface — since #1211 it is a route,
     and the rail only navigates. `AppShell` owns the seam, because the
     cove page's `+` opens the same one. All the row reports is which cove. */
  it('starts a wave in its own cove from the row, without navigating into it', async () => {
    const onNewWave = vi.fn();
    const onGo = vi.fn();
    renderSidebar({
      coves: [cove(), cove({ id: 'c2', name: 'Reading', sort: 2 })],
      wavesByCove: new Map([['c1', []], ['c2', []]]),
      onNewWave,
      onGo,
    });
    await userEvent.click(screen.getByRole('button', { name: 'New wave in Reading' }));
    expect(onNewWave.mock.calls).toEqual([['c2']]);
    expect(onGo).not.toHaveBeenCalled();
  });
});

describe('new cove', () => {
  it('submits on Enter with a palette colour, and cancels on Escape', async () => {
    const onCreateCove = vi.fn();
    renderSidebar({ onCreateCove });

    await userEvent.click(screen.getByRole('button', { name: 'New cove' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Cove name' }), 'Reading{Escape}');
    expect(onCreateCove).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'New cove' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Cove name' }), 'Reading{Enter}');
    expect(onCreateCove).toHaveBeenCalledTimes(1);
    const [name, color] = onCreateCove.mock.calls[0] as [string, string];
    expect(name).toBe('Reading');
    expect(COVE_PALETTE).toContain(color);
  });

  it('submits on blur', async () => {
    const onCreateCove = vi.fn();
    renderSidebar({ onCreateCove });
    await userEvent.click(screen.getByRole('button', { name: 'New cove' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Cove name' }), 'Later');
    await userEvent.click(screen.getByRole('button', { name: 'Work' }));
    expect(onCreateCove.mock.calls.map((call) => (call as string[])[0])).toEqual(['Later']);
  });
});

describe('destructive confirms', () => {
  it('deletes a wave only after Confirm, and nothing on Cancel', async () => {
    const onDeleteWave = vi.fn();
    renderSidebar({ waves: [wave({ id: 'w1', title: 'Task' })], onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    expect(screen.getByRole('dialog', { name: 'Delete this wave?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onDeleteWave).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    expect(onDeleteWave).toHaveBeenCalledWith('w1', expect.any(AbortSignal));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('deletes a cove only after Confirm', async () => {
    const onDeleteCove = vi.fn();
    renderSidebar({ onDeleteCove });

    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    // §6.13 / CR-5a — the title names the cove, and Confirm stays blocked until
    // the name is reproduced. Deleting a cove cascades to every wave inside it;
    // it is the one operation in the product that earns a typed confirm, and
    // this rail entry shares that dialog with the cove page's header button.
    expect(screen.getByRole('dialog', { name: 'Delete Work?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));
    expect(onDeleteCove).not.toHaveBeenCalled();

    await userEvent.type(screen.getByLabelText('Type Work to confirm.'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));
    expect(onDeleteCove).toHaveBeenCalledWith('c1', expect.any(AbortSignal));
  });

  it('states that the cascade count is unknown when the cove wave query has no data', async () => {
    renderSidebar({ wavesByCove: new Map() });
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(screen.getByRole('dialog').textContent).toContain('The number of waves is not available.');
    expect(screen.getByRole('dialog').textContent).not.toContain('deletes 0 waves');
  });

  it('describes deletion of a genuinely empty cove without claiming it deletes zero waves', async () => {
    renderSidebar({ wavesByCove: new Map([['c1', []]]) });
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(screen.getByRole('dialog').textContent).toContain('This deletes the cove.');
    expect(screen.getByRole('dialog').textContent).not.toContain('deletes 0 waves');
  });

  it('keeps the confirm mounted while the delete is in flight and clears it on rejection', async () => {
    let reject: (reason: Error) => void = () => {};
    let signal: AbortSignal | undefined;
    const onDeleteWave = vi.fn((_id: string, requestSignal: AbortSignal) => {
      signal = requestSignal;
      return new Promise<void>((_resolve, rejectFn) => { reject = rejectFn; });
    });
    renderSidebar({ waves: [wave({ id: 'w1', title: 'Task' })], onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
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
    expect(screen.queryByRole('dialog', { name: 'Delete this wave?' })).toBeNull();
    expect(signal?.aborted).toBe(true);

    reject(new DOMException('aborted', 'AbortError'));
    await screen.findByRole('button', { name: 'Delete Task' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('can delete a second target immediately after canceling the first request', async () => {
    const deleted: string[] = [];
    const onDeleteWave = vi.fn((id: string, signal: AbortSignal) => {
      deleted.push(id);
      if (id !== 'w1') return Promise.resolve();
      return new Promise<void>((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      });
    });
    renderSidebar({ waves: [wave({ id: 'w1', title: 'Alpha' }), wave({ id: 'w2', title: 'Beta' })], onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Alpha' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete Beta' }));
    expect(screen.getByRole('button', { name: 'Delete wave' }).getAttribute('aria-busy')).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    expect(deleted).toEqual(['w1', 'w2']);
  });
});

describe('pin', () => {
  it('pins straight from the row without a confirm', async () => {
    const onSetPinned = vi.fn();
    renderSidebar({ waves: [wave({ id: 'w1', title: 'Task' })], onSetPinned });
    await userEvent.click(screen.getByRole('button', { name: 'Pin Task' }));
    expect(onSetPinned.mock.calls).toEqual([['w1', true]]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});

describe('user menu', () => {
  it('opens Settings and Sign out from the avatar, and calls the injected callbacks', async () => {
    const onOpenSettings = vi.fn();
    const onSignOut = vi.fn();
    renderSidebar({ onOpenSettings, onSignOut, userLabel: 'Kenji Xie' });

    const avatar = screen.getByRole('button', { name: 'Account menu for Kenji Xie' });
    expect(avatar.textContent).toBe('KX');
    await userEvent.click(avatar);
    // Theme cycles in place (system -> light -> dark) rather than opening a
    // submenu: three modes is not enough to earn one, and the current mode has
    // to be readable without opening anything further.
    expect(screen.getAllByRole('menuitem').map((node) => node.textContent))
      .toEqual(['Theme: system (light)', 'Settings', 'Sign out']);

    await userEvent.click(screen.getByRole('menuitem', { name: 'Settings' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

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
    renderSidebar({ waves: [wave({ title: 'Inside' })], onToggleCollapsed });
    const toggle = screen.getByRole('button', { name: 'Collapse sidebar' });
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    await userEvent.click(toggle);
    expect(onToggleCollapsed).toHaveBeenCalledTimes(1);
    // Nothing changed here, because nothing here owns it.
    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
  });

  it('drops to an icon strip when told it is collapsed, and stays navigable', () => {
    const { update } = renderSidebar({ waves: [wave({ title: 'Inside' })] });
    update({ collapsed: true });

    // No section labels: 11px uppercase does not fit in 44px, and the strip
    // answers "where am I", not "what is there".
    expect(screen.queryAllByRole('heading')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /^Wave Inside/ })).toBeNull();
    // The cove is still reachable, named for assistive tech and initialled for
    // sighted users — a letter, not one of eight cove hues, because §7.5 keeps
    // this surface greyscale apart from the current location and "waiting".
    const item = screen.getByRole('button', { name: 'Work' });
    expect(item.textContent).toBe('W');
    expect(screen.getByRole('button', { name: 'Account menu for You' })).toBeTruthy();
    const expand = screen.getByRole('button', { name: 'Expand sidebar' });
    expect(expand.getAttribute('aria-expanded')).toBe('false');
    expect(expand.querySelector('[aria-hidden="true"]')).toBeTruthy();

    update({ collapsed: false });
    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
  });

  it('shows the waiting count as the strip\'s only figure, with no dot beside it', () => {
    const { update } = renderSidebar({
      waves: [wave({ id: 'a', lifecycle: 'blocked' }), wave({ id: 'b', lifecycle: 'draft' })],
    });
    update({ collapsed: true });
    const count = screen.getByLabelText('1 waiting on you');
    expect(count.textContent).toBe('1');
  });
});
