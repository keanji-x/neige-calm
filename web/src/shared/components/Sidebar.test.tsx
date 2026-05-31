import { describe, it, expect, vi, afterEach } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import type { Cove, Route, Wave } from '../../types';
import { Sidebar } from './Sidebar';

afterEach(() => {
  cleanup();
  window.localStorage.clear();
});

const STUB_SESSION = {
  userId: 'u-test',
  displayName: 'Test User',
  role: 'owner',
  sessionId: 's-test',
};

function wrap(children: ReactNode) {
  return (
    <SessionContext.Provider value={STUB_SESSION}>
      {children}
    </SessionContext.Provider>
  );
}

function makeCove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9', ...overrides };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'c1',
    title: 'Harbor cleanup',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    ...overrides,
  };
}

function renderSidebar({
  coves = [makeCove()],
  waves = [makeWave()],
  route = { name: 'today' },
  onGo = () => {},
  onPinWave,
  onDeleteWave,
}: {
  coves?: Cove[];
  waves?: Wave[];
  route?: Route;
  onGo?: (r: Route) => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
  onDeleteWave?: (waveId: string) => void | Promise<void>;
} = {}) {
  return render(
    wrap(
      <Sidebar
        coves={coves}
        waves={waves}
        route={route}
        onGo={onGo}
        onPinWave={onPinWave}
        onDeleteWave={onDeleteWave}
      />,
    ),
  );
}

async function expandAtlas(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('button', { name: 'Expand cove Atlas' }));
  return screen.getByRole('group', { name: 'Waves in Atlas' });
}

describe('Sidebar wave delete', () => {
  it('Per-row × on a sidebar WaveRow shows a confirm dialog and calls onDeleteWave on confirm', async () => {
    const user = userEvent.setup();
    const onDeleteWave = vi.fn();
    renderSidebar({ onDeleteWave });

    const inline = await expandAtlas(user);
    await user.click(
      within(inline).getByRole('button', { name: 'Delete wave "Harbor cleanup"' }),
    );

    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    expect(dialog).toHaveTextContent('Delete wave "Harbor cleanup"?');
    await user.click(within(dialog).getByRole('button', { name: 'Delete wave' }));

    expect(screen.queryByRole('dialog', { name: 'Delete wave?' })).toBeNull();
    expect(onDeleteWave).toHaveBeenCalledTimes(1);
    expect(onDeleteWave).toHaveBeenCalledWith('w1');
  });

  it('Cancel closes the dialog without invoking onDeleteWave', async () => {
    const user = userEvent.setup();
    const onDeleteWave = vi.fn();
    renderSidebar({ onDeleteWave });

    const inline = await expandAtlas(user);
    await user.click(
      within(inline).getByRole('button', { name: 'Delete wave "Harbor cleanup"' }),
    );
    const dialog = screen.getByRole('dialog', { name: 'Delete wave?' });
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));

    expect(screen.queryByRole('dialog', { name: 'Delete wave?' })).toBeNull();
    expect(onDeleteWave).not.toHaveBeenCalled();
  });

  it('Pin button is on the left of the row (DOM order: pin → title → delete)', () => {
    const wave = makeWave({ pinnedAt: 1000 });
    renderSidebar({
      waves: [wave],
      onPinWave: vi.fn(),
      onDeleteWave: vi.fn(),
    });

    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const row = within(pinned)
      .getByText('Harbor cleanup')
      .closest('.side-wave-row');
    expect(row).not.toBeNull();

    const buttons = within(row as HTMLElement).getAllByRole('button');
    expect(buttons).toHaveLength(3);
    expect(buttons[0]).toHaveAccessibleName('Unpin wave');
    expect(buttons[1]).toHaveTextContent('Harbor cleanup');
    expect(buttons[2]).toHaveAccessibleName('Delete wave "Harbor cleanup"');
  });
});
