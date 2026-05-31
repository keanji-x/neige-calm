// Component-level tests for the Sidebar pin-wave feature.
//
// Pinned waves appear in a dedicated "Pinned" section below "Waiting on you".
// A pin/unpin button is revealed on row hover. Pinned waves that need
// attention also appear in "Waiting on you" and increment cove warn badges.

import { describe, it, expect, vi, afterEach } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
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
    const wave = makeWave({ lifecycle: 'draft', anyCardNeedsInput: false, pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByText('My wave')).toBeTruthy();
  });

  it('renders the fallback label for a pinned wave with an empty title', () => {
    const wave = makeWave({
      title: '',
      lifecycle: 'draft',
      anyCardNeedsInput: false,
      pinnedAt: 1000,
    });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toHaveTextContent('Untitled wave');
  });

  it('pinned wave appears in both Pinned and Waiting on you', () => {
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    expect(pinned).toBeTruthy();
    expect(waiting).toBeTruthy();
    expect(pinned).toHaveTextContent('My wave');
    expect(waiting).toHaveTextContent('My wave');
  });

  it('renders Waiting on you before Pinned when a wave is both pinned and waiting', () => {
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    expect(
      waiting.compareDocumentPosition(pinned) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it('unpinned wave that needs attention appears only in Waiting on you', () => {
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.queryByRole('region', { name: 'Pinned' })).toBeNull();
    expect(screen.getByRole('region', { name: 'Waiting on you' })).toBeTruthy();
  });

  it('waiting wave renders Waiting on you as an attention zone', () => {
    const wave = makeWave({ lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    expect(waiting.classList.contains('attn-zone')).toBe(true);
  });

  it('calls onPinWave(id, false) when pin button is clicked on a pinned wave', () => {
    const onPinWave = vi.fn();
    const wave = makeWave({
      id: 'w-pin',
      lifecycle: 'draft',
      anyCardNeedsInput: false,
      pinnedAt: 1000,
    });
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
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const buttons = Array.from(pinned.querySelectorAll<HTMLButtonElement>('button.side-wave'));
    // "Second" (pinnedAt=500) must come before "First" (pinnedAt=1000)
    expect(buttons[0]).toHaveTextContent('Second');
    expect(buttons[1]).toHaveTextContent('First');
  });
});

describe('Sidebar per-cove badge parity with Waiting section', () => {
  it('pinned blocked wave increments the cove warn waiting badge', () => {
    const wave = makeWave({ id: 'w-pinned-blocked', lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    expect(screen.getByRole('region', { name: 'Pinned' })).toBeTruthy();
    expect(screen.getByRole('region', { name: 'Waiting on you' })).toBeTruthy();
    const badge = document.querySelector('.cove-nav-badge.warn');
    expect(badge).toBeTruthy();
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

  it('pinned attention row carries the attention class for warn title styling', () => {
    const wave = makeWave({ id: 'w-pinned-attention', lifecycle: 'blocked', pinnedAt: 1000 });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const pinned = screen.getByRole('region', { name: 'Pinned' });
    const row = pinned.querySelector('.side-wave-row.attention');
    expect(row).toBeTruthy();
    expect(row?.querySelector('.side-wave-title')).toHaveTextContent('My wave');
  });

  it('waiting attention row carries the attention class for warn title styling', () => {
    const wave = makeWave({ id: 'w-attention', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave])} />));
    const waiting = screen.getByRole('region', { name: 'Waiting on you' });
    const row = waiting.querySelector('.side-wave-row.attention');
    expect(row).toBeTruthy();
    expect(row?.querySelector('.side-wave-title')).toHaveTextContent('My wave');
  });

  it('inline cove row carries the attention class for warn title styling', () => {
    const onPinWave = vi.fn();
    const wave = makeWave({ id: 'w-inline-attention', lifecycle: 'blocked', pinnedAt: null });
    render(wrap(<Sidebar {...sidebarProps([wave], onPinWave)} />));

    fireEvent.click(screen.getByRole('button', { name: /Expand cove Atlas/ }));

    const inline = screen.getByRole('group', { name: 'Waves in Atlas' });
    expect(within(inline).getByText('My wave')).toBeTruthy();
    expect(inline.querySelector('.side-wave-row.attention .side-wave-title')).toBeTruthy();
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
