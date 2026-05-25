// Component-level tests for the Sidebar pin-wave feature.
//
// Pinned waves appear in a dedicated "Pinned" section above "Waiting on you".
// A pin/unpin button is revealed on row hover. Waves excluded from "Waiting
// on you" when they are already pinned (no double render).

import { describe, it, expect, vi, afterEach } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { SessionContext } from '../../app/SessionProvider';
import { Sidebar } from './Sidebar';
import type { Cove, Wave } from '../../types';

afterEach(cleanup);

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

function makeCove(id = 'c1'): Cove {
  return { id, name: 'Atlas', subtitle: '', color: '#5a9' };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'c1',
    title: 'My wave',
    lifecycle: 'blocked',
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

function sidebarProps(
  waves: Wave[],
  onPinWave?: (id: string, pin: boolean) => void,
) {
  return {
    coves: [makeCove()],
    waves,
    route: { name: 'today' } as const,
    onGo: () => {},
    onPinWave,
  };
}

describe('Sidebar pinned section', () => {
  it('renders no Pinned section when all waves are unpinned', () => {
    const wave = makeWave({ lifecycle: 'draft', anyCardNeedsInput: false });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull();
  });

  it('renders a Pinned section when a wave has pinnedAt set', () => {
    const wave = makeWave({ pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByText('My wave')).toBeTruthy();
  });

  it('pinned wave does not appear in Waiting on you', () => {
    // lifecycle=blocked + pinnedAt set → waiting predicate matches but
    // pinned waves are filtered out of the Waiting section.
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    expect(pinned).toBeTruthy();
    expect(screen.queryByRole('region', { name: 'Waiting on you' })).toBeNull();
  });

  it('unpinned wave that needs attention appears only in Waiting on you', () => {
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull();
    expect(screen.getByRole('region', { name: 'Waiting on you' })).toBeTruthy();
  });

  it('calls onPinWave(id, false) when pin button is clicked on a pinned wave', () => {
    const onPinWave = vi.fn();
    const wave = makeWave({ id: 'w-pin', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave], onPinWave)} />));
    const btn = screen.getByRole('button', { name: 'Unpin wave' });
    fireEvent.click(btn);
    expect(onPinWave).toHaveBeenCalledWith('w-pin', false);
  });

  it('calls onPinWave(id, true) when pin button is clicked on an unpinned wave', () => {
    const onPinWave = vi.fn();
    // waiting wave = blocked lifecycle
    const wave = makeWave({ id: 'w-unpin', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave], onPinWave)} />));
    const btn = screen.getByRole('button', { name: 'Pin wave' });
    fireEvent.click(btn);
    expect(onPinWave).toHaveBeenCalledWith('w-unpin', true);
  });

  it('sorts pinned waves by pinnedAt ascending', () => {
    const w1 = makeWave({ id: 'w1', title: 'First', pinnedAt: 1000 });
    const w2 = makeWave({ id: 'w2', title: 'Second', pinnedAt: 500 });
    render(wrap(<Sidebar {...sidebarProps([w1, w2])} />));
    const buttons = screen.getAllByRole('button', { name: /First|Second/ });
    // "Second" (pinnedAt=500) must come before "First" (pinnedAt=1000)
    expect(buttons[0]).toHaveTextContent('Second');
    expect(buttons[1]).toHaveTextContent('First');
  });
});

describe('Sidebar per-cove badge parity with Waiting section', () => {
  it('pinned blocked wave does not increment the cove red waiting badge', () => {
    // A wave that is pinned AND blocked should appear in Pinned, not in the
    // Waiting section, and the cove badge should show no red waiting count.
    const wave = makeWave({ id: 'w-pinned-blocked', lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    // Pinned section shows up
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    // Waiting section is absent
    expect(screen.queryByRole('region', { name: 'Waiting on you' })).toBeNull();
    // Cove badge: no warn badge (no unpinned waiting wave) — the badge should
    // show the muted total count (1), not a warn (red) count.
    // The muted badge reads "1" (total cove waves) and has className "muted".
    const badge = document.querySelector('.cove-nav-badge');
    expect(badge).toBeTruthy();
    expect(badge?.classList.contains('warn')).toBe(false);
    expect(badge?.classList.contains('muted')).toBe(true);
    expect(badge?.textContent).toBe('1');
  });

  it('unpinned blocked wave increments the cove red waiting badge', () => {
    const wave = makeWave({ id: 'w-unblocked', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const badge = document.querySelector('.cove-nav-badge');
    expect(badge).toBeTruthy();
    expect(badge?.classList.contains('warn')).toBe(true);
    expect(badge?.textContent).toBe('1');
  });
});

describe('Sidebar WaveRow cove-name span', () => {
  it('wave with no matching cove renders without the cove text span', () => {
    // coveId does not match any cove in the list → orphan wave
    const wave = makeWave({ id: 'w-orphan', coveId: 'nonexistent', lifecycle: 'blocked', pinnedAt: null });
    render(
      wrap(
        <Sidebar
          coves={[makeCove('c1')]}
          waves={[wave]}
          route={{ name: 'today' }}
          onGo={() => {}}
        />,
      ),
    );
    // No .side-wave-cove span rendered when cove is not found.
    expect(document.querySelector('.side-wave-cove')).toBeNull();
    // The wave nav button is still present.
    expect(screen.getByRole('button', { name: /My wave/i })).toBeTruthy();
  });
});
