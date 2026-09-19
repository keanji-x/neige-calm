import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { CardActivity } from '../../../../../core/domain/activity.ts';
import { cardWireSchema } from '../../../../../core/domain/track.ts';
import { createCardHost } from '../host.ts';
import { createCardRegistry } from '../registry.ts';
import { BoardHost } from '../ui/board-host.tsx';
import { partitionTrackCards } from './headless-filter.ts';
import { registerAvailableBuiltinCards } from './register.ts';

/*
 * #1722 S2b r1 (Codex P2-3) — the terminal head's spoken verdict.
 *
 * A *connected* head is the case that matters: once the connection is up the
 * head prints no words of its own (`terminal-lifecycle.test.tsx` covers the
 * `Connecting…` / `Session exited.` words), so the indicator is the only thing
 * left and, decorative by contract, it would leave a working terminal with no
 * accessible "in motion" fact. The real surface never connects under jsdom
 * (no socket, no `ServerHello`), so this file — and only this file — replaces
 * it with one that reports `connected` on mount; the lifecycle suite keeps the
 * real surface and its `Connecting…` reading.
 */
vi.mock('../../terminal/surface.tsx', async () => {
  const { createElement, useEffect } = await import('react');
  return {
    TerminalSurface: ({ onStatusChange }: { onStatusChange?: (status: 'connected') => void }) => {
      useEffect(() => { onStatusChange?.('connected'); }, [onStatusChange]);
      return createElement('div', { 'data-testid': 'connected-surface' });
    },
  };
});

afterEach(cleanup);

/** The card head (`CardHead`'s root, marked as the drag handle) inside the mounted cell. */
const head = (): HTMLElement => {
  const found = document.querySelector<HTMLElement>('[data-nc-card-cell] [data-nc-card-drag]');
  if (found === null) throw new Error('no card head');
  return found;
};

function mountConnected(kind: string, activity: CardActivity | null) {
  const wire = cardWireSchema.parse({
    id: 'card-1', track_id: 'track-1', kind, title: null, sort: 1,
    payload: {}, deletable: true, created_at: 1, updated_at: 2,
    runtime: { worker_session_id: 'run-1', kind: 'terminal', status: 'running', terminal_id: 'pty' },
  });
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const card = partitionTrackCards(registry, [wire]).visible[0]?.card;
  if (card === undefined) throw new Error('Missing built-in card');
  return render(<BoardHost host={createCardHost(registry)} items={[
    { card, title: kind, originalIndex: 0, deletable: true, activity },
  ]} visible activeCardId="card-1" />);
}

describe.each(['terminal', 'codex', 'claude'])('%s terminal head, connected', (kind) => {
  it('speaks the kernel verdict beside its marker', async () => {
    mountConnected(kind, 'working');
    await screen.findByTestId('connected-surface');
    expect(screen.queryByText('Connecting…')).toBeNull();
    expect(within(head()).getByText('Working')).toBeTruthy();
    const marker = head().querySelector('[data-nc-activity="working"]');
    expect(marker).not.toBeNull();
    expect(marker?.nextElementSibling).toBe(within(head()).getByText('Working'));
    expect(head().querySelectorAll('[data-nc-activity]')).toHaveLength(1);
  });

  it('speaks a request for input', async () => {
    mountConnected(kind, 'input');
    await screen.findByTestId('connected-surface');
    expect(within(head()).getByText('Needs input')).toBeTruthy();
    expect(head().querySelector('[data-nc-activity="attention"]')).not.toBeNull();
  });

  it('says nothing at all with no verdict', async () => {
    mountConnected(kind, null);
    await screen.findByTestId('connected-surface');
    expect(screen.queryByText('Connecting…')).toBeNull();
    expect(head().querySelector('[data-nc-activity]')).toBeNull();
    expect(head().querySelector('[role="status"]')).toBeNull();
    expect(within(head()).queryByText(/^(Working|Needs input|Needs attention|Unread updates)$/)).toBeNull();
  });
});
