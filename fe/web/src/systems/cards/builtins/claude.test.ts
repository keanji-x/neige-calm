import { describe, expect, it } from 'vitest';

import { createCardRegistry } from '../registry.js';
import { CLAUDE_CARD_ENTRY } from './claude.ts';
import { partitionTrackCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(id: string, kind: string, payload: unknown) {
  return {
    id, kind, track_id: 'w1', title: null, sort: 0, payload,
    deletable: true, created_at: 0, updated_at: 0,
  };
}

describe('CLAUDE_CARD_ENTRY', () => {
  it('resolves kernel claude cards, including before terminal_id is projected', () => {
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c1', kind: 'claude', payload: { terminal_id: 't1' },
    })).toEqual({
      type: 'claude', id: 'c1', title: null, terminalId: 't1', sessionState: null, cwd: null, gateCwd: null,
      ownerCreated: false, workerSessionId: null, runningVersion: null,
    });
    // The kernel projects `terminal_id` on read; a card observed between mint and projection resolves with a null terminal rather than not at all.
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c2', kind: 'claude', payload: { goal: 'do the thing' },
    })).toEqual({
      type: 'claude', id: 'c2', title: null, terminalId: null, sessionState: null, cwd: null, gateCwd: null,
      ownerCreated: false, workerSessionId: null, runningVersion: null,
    });
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c3', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  it('reads the owner marker, the runtime and the version its child started on', () => {
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c4', kind: 'claude', payload: { owner_created: true },
      runtime: { worker_session_id: 'rt-2', kind: 'claude', status: 'running', terminal_id: 't4', claude_version: '2.1.259' },
    })).toMatchObject({ ownerCreated: true, workerSessionId: 'rt-2', runningVersion: '2.1.259', terminalId: 't4', sessionState: 'running' });
    // Only the kernel's own `true` marks an owner card; a task worker's card carries none.
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({ id: 'c5', kind: 'claude', payload: { owner_created: 'true' } })?.ownerCreated)
      .toBe(false);
  });

  it('is created atomically from the add menu, through the track route rather than here (#2516)', () => {
    expect(CLAUDE_CARD_ENTRY.create.mode).toBe('atomic');
    expect(CLAUDE_CARD_ENTRY.addPanel).toEqual({ label: 'claude' });
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('claude')?.headless).toBe(false);
    expect(registry.resolve({ id: 'c1', kind: 'claude', payload: { terminal_id: 't9' } })?.type)
      .toBe('claude');
  });

  it('lands claude cards in the visible partition beside terminal cards', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const { visible, unknown } = partitionTrackCards(registry, [
      wire('k-claude', 'claude', { terminal_id: 't1' }),
      wire('k-term', 'terminal', { terminal_id: 't2' }),
    ]);
    expect(visible.map((slot) => slot.card.type)).toEqual(['claude', 'terminal']);
    expect(unknown).toEqual([]);
  });
});
