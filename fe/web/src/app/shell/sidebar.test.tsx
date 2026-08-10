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
          onSetPinned={merged.onSetPinned ?? vi.fn()}
          onDeleteWave={merged.onDeleteWave ?? vi.fn()}
          onOpenSettings={merged.onOpenSettings ?? vi.fn()}
          onSignOut={merged.onSignOut ?? vi.fn()}
          userLabel={merged.userLabel}
        />
      </ThemeProvider>
    );
  };
  const result = render(build({}));
  return { ...result, update: (overrides: Partial<Props>) => result.rerender(build(overrides)) };
}

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

describe('cove badge', () => {
  it('counts waiting waves in preference to the total, and shows nothing when empty', () => {
    const waves = [
      wave({ id: 'a', lifecycle: 'blocked' }),
      wave({ id: 'b', lifecycle: 'draft' }),
      wave({ id: 'c', lifecycle: 'draft' }),
    ];
    const { update } = renderSidebar({ waves, wavesByCove: new Map([['c1', waves]]) });
    // Three waves, one blocked: the badge is the waiting count, not the total.
    expect(screen.getByRole('button', { name: 'Work' }).textContent).toBe('Work1');

    const quiet = [wave({ id: 'b', lifecycle: 'draft' }), wave({ id: 'c', lifecycle: 'draft' })];
    update({ waves: quiet, wavesByCove: new Map([['c1', quiet]]) });
    expect(screen.getByRole('button', { name: 'Work' }).textContent).toBe('Work2');

    update({ waves: [], wavesByCove: new Map([['c1', []]]) });
    expect(screen.getByRole('button', { name: 'Work' }).textContent).toBe('Work');
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
    expect(onDeleteWave.mock.calls).toEqual([['w1']]);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('deletes a cove only after Confirm', async () => {
    const onDeleteCove = vi.fn();
    renderSidebar({ onDeleteCove });

    await userEvent.click(screen.getByRole('button', { name: 'Delete cove Work' }));
    expect(screen.getByRole('dialog', { name: 'Delete this cove?' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Delete cove' }));
    expect(onDeleteCove.mock.calls).toEqual([['c1']]);
  });

  it('keeps the confirm mounted while the delete is in flight and clears it on rejection', async () => {
    let reject: (reason: Error) => void = () => {};
    const onDeleteWave = vi.fn(() => new Promise<void>((_resolve, rejectFn) => { reject = rejectFn; }));
    renderSidebar({ waves: [wave({ id: 'w1', title: 'Task' })], onDeleteWave });

    await userEvent.click(screen.getByRole('button', { name: 'Delete Task' }));
    await userEvent.click(screen.getByRole('button', { name: 'Delete wave' }));
    // Confirm disabled, Cancel still an exit, dialog still mounted.
    expect(screen.getByRole('button', { name: 'Delete wave' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Cancel' }).hasAttribute('disabled')).toBe(false);

    reject(new Error('boom'));
    await screen.findByRole('button', { name: 'Delete Task' });
    expect(screen.queryByRole('dialog')).toBeNull();
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
    expect(screen.getAllByRole('menuitem').map((node) => node.textContent)).toEqual(['Settings', 'Sign out']);

    await userEvent.click(screen.getByRole('menuitem', { name: 'Settings' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole('button', { name: 'Account menu for Kenji Xie' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Sign out' }));
    expect(onSignOut).toHaveBeenCalledTimes(1);
  });
});

describe('collapse toggle', () => {
  it('drops the rail to an icon strip and keeps the toggle reachable', async () => {
    renderSidebar({ waves: [wave({ title: 'Inside' })] });
    await userEvent.click(screen.getByRole('button', { name: 'Collapse sidebar' }));

    expect(screen.queryAllByRole('heading')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: /^Wave Inside/ })).toBeNull();
    // The cove is still reachable as an icon, and the rail can come back.
    expect(screen.getByRole('button', { name: 'Work' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Expand sidebar' }));
    expect(screen.getByRole('heading', { name: 'Coves' })).toBeTruthy();
  });
});
