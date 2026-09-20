import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { CardActivity } from '../../../../../core/domain/activity.ts';
import { cardWireSchema } from '../../../../../core/domain/track.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { partitionTrackCards } from './headless-filter.ts';
import { registerAvailableBuiltinCards } from './register.ts';

afterEach(cleanup);

/* The head's activity indicator is decorative by contract; the head's words are the accessible facts. */
const headIndicator = () => document.querySelector('[data-nc-card-cell] [data-nc-activity]');

function mountCard(
  kind: string, runtime?: { status: string; terminal_id?: string }, payload: unknown = {},
  activity: CardActivity | null = null,
) {
  const wire = cardWireSchema.parse({
    id: 'card-1', track_id: 'track-1', kind, title: null, sort: 1,
    payload, deletable: true, created_at: 1, updated_at: 2,
    ...(runtime === undefined ? {} : { runtime: { worker_session_id: 'run-1', kind: 'terminal', ...runtime } }),
  });
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const card = partitionTrackCards(registry, [wire]).visible[0]?.card;
  if (card === undefined) throw new Error('Missing built-in card');
  return render(<BoardHost host={createCardHost(registry)} items={[
    { card, title: kind, originalIndex: 0, deletable: true, activity },
  ]} visible activeCardId="card-1" />);
}

describe.each(['terminal', 'codex', 'claude'])('%s terminal lifecycle after refresh', (kind) => {
  it('shows an exited session instead of an endless startup', () => {
    mountCard(kind, { status: 'exited' });
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(screen.queryByText(`Starting ${kind}…`)).toBeNull();
    expect(headIndicator()).toBeNull();
  });

  it('keeps both execution directories visible after the session exits', () => {
    mountCard(kind, { status: 'exited' }, {
      cwd: '/repo/worker-checkout', gate_cwd: '/repo/gate-checkout', terminal_id: 'stale-pty',
    });
    expect(screen.getByText('/repo/worker-checkout')).toBeTruthy();
    expect(screen.getByText('/repo/gate-checkout')).toBeTruthy();
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(headIndicator()).toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="stale-pty"]')).toBeNull();
  });

  it('shows a failed session instead of an endless startup', () => {
    mountCard(kind, { status: 'failed' });
    expect(screen.getByText('Session failed.')).toBeTruthy();
  });

  it('only announces startup when the runtime is starting', () => {
    mountCard(kind, { status: 'starting' });
    expect(screen.getByText(`Starting ${kind}…`)).toBeTruthy();
  });

  it('does not promise startup when there is no session', () => {
    mountCard(kind);
    expect(screen.getByText('No terminal session available.')).toBeTruthy();
  });

  it('ignores a stale payload terminal when the runtime has already exited', () => {
    mountCard(kind, { status: 'exited' }, { terminal_id: 'stale-pty' });
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(document.querySelector('[data-nc-terminal-id="stale-pty"]')).toBeNull();
    expect(headIndicator()).toBeNull();
  });

  it('uses the runtime terminal identity instead of a stale payload identity', () => {
    mountCard(kind, { status: 'running', terminal_id: 'current-pty' }, { terminal_id: 'stale-pty' });
    expect(document.querySelector('[data-nc-terminal-id="current-pty"]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="stale-pty"]')).toBeNull();
    expect(screen.getByText('Connecting…')).toBeTruthy();
    expect(headIndicator()).toBeNull();
  });

  it('still resolves legacy payload identity when no runtime is projected', () => {
    mountCard(kind, undefined, { terminal_id: 'legacy-pty' });
    expect(document.querySelector('[data-nc-terminal-id="legacy-pty"]')).not.toBeNull();
  });

  it('shows replaced sessions without promising a new terminal', () => {
    mountCard(kind, { status: 'superseded' });
    expect(screen.getByText('Session replaced.')).toBeTruthy();
  });

  // jsdom never connects, so the surface sits at `Connecting…` throughout — the indicator does not wait for it.
  it('terminal head paints activity.cards, not runtime.status', () => {
    const { unmount } = mountCard(kind, { status: 'running', terminal_id: 'pty' });
    expect(screen.getByText('Connecting…')).toBeTruthy();
    expect(headIndicator()).toBeNull();
    unmount();

    const working = mountCard(kind, { status: 'running', terminal_id: 'pty' }, {}, 'working');
    expect(screen.getByText('Connecting…')).toBeTruthy();
    expect(headIndicator()?.getAttribute('data-nc-activity')).toBe('working');
    working.unmount();

    // A failed session can be red AND say so; neither stands in for the other.
    mountCard(kind, { status: 'failed' }, {}, 'failed');
    expect(screen.getByText('Session failed.')).toBeTruthy();
    expect(headIndicator()?.getAttribute('data-nc-activity')).toBe('failed');
    expect(headIndicator()?.parentElement).toBe(screen.getByText('Session failed.').parentElement);
  });
});
