import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { cardWireSchema } from '../../../../../core/domain/track.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { partitionTrackCards } from './headless-filter.ts';
import { registerAvailableBuiltinCards } from './register.ts';

afterEach(cleanup);

function mountCard(kind: string, runtime?: { status: string; terminal_id?: string }, payload: unknown = {}) {
  const wire = cardWireSchema.parse({
    id: 'card-1', track_id: 'track-1', kind, title: null, sort: 1,
    payload, deletable: true, created_at: 1, updated_at: 2,
    ...(runtime === undefined ? {} : { runtime: { runtime_id: 'run-1', kind: 'terminal', ...runtime } }),
  });
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const card = partitionTrackCards(registry, [wire]).visible[0]?.card;
  if (card === undefined) throw new Error('Missing built-in card');
  return render(<BoardHost host={createCardHost(registry)} items={[
    { card, title: kind, originalIndex: 0, deletable: true },
  ]} visible activeCardId="card-1" />);
}

describe.each(['terminal', 'codex', 'claude'])('%s terminal lifecycle after refresh', (kind) => {
  it('shows an exited session instead of an endless startup', () => {
    mountCard(kind, { status: 'exited' });
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(screen.queryByText(`Starting ${kind}…`)).toBeNull();
    expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
  });

  it('keeps both execution directories visible after the session exits', () => {
    mountCard(kind, { status: 'exited' }, {
      cwd: '/repo/worker-checkout', gate_cwd: '/repo/gate-checkout', terminal_id: 'stale-pty',
    });
    expect(screen.getByText('/repo/worker-checkout')).toBeTruthy();
    expect(screen.getByText('/repo/gate-checkout')).toBeTruthy();
    expect(screen.getByText('Session exited.')).toBeTruthy();
    expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
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
    expect(screen.queryByRole('img', { name: 'status Working' })).toBeNull();
  });

  it('uses the runtime terminal identity instead of a stale payload identity', () => {
    mountCard(kind, { status: 'running', terminal_id: 'current-pty' }, { terminal_id: 'stale-pty' });
    expect(document.querySelector('[data-nc-terminal-id="current-pty"]')).not.toBeNull();
    expect(document.querySelector('[data-nc-terminal-id="stale-pty"]')).toBeNull();
    expect(screen.getByRole('img', { name: 'status Working' })).toBeTruthy();
  });

  it('still resolves legacy payload identity when no runtime is projected', () => {
    mountCard(kind, undefined, { terminal_id: 'legacy-pty' });
    expect(document.querySelector('[data-nc-terminal-id="legacy-pty"]')).not.toBeNull();
  });

  it('shows replaced sessions without promising a new terminal', () => {
    mountCard(kind, { status: 'superseded' });
    expect(screen.getByText('Session replaced.')).toBeTruthy();
  });
});
