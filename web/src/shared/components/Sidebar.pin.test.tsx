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

function sidebarProps(waves: Wave[], onPinWave?: (id: string, pin: boolean) => void) {
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
