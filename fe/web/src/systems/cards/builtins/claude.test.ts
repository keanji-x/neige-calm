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
    })).toEqual({ type: 'claude', id: 'c1', title: null, terminalId: 't1', sessionState: null, cwd: null, gateCwd: null });
    // The kernel projects `terminal_id` on read; a card observed between mint
    // and projection resolves with a null terminal rather than not at all.
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c2', kind: 'claude', payload: { goal: 'do the thing' },
    })).toEqual({ type: 'claude', id: 'c2', title: null, terminalId: null, sessionState: null, cwd: null, gateCwd: null });
    expect(CLAUDE_CARD_ENTRY.fromKernel?.({
      id: 'c3', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  it('is kernel-minted-only — worker cards come from a task row, not a gesture', () => {
    expect(CLAUDE_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('claude')?.headless).toBe(false);
    expect(registry.resolve({ id: 'c1', kind: 'claude', payload: { terminal_id: 't9' } })?.type)
      .toBe('claude');
  });

  /*
   * The bug this card exists to fix. A claude card with no adapter fell into
   * `unknown`, so `app/router` left it out of `gridItems`, `knownCard` stayed
   * false and the route effect replaced the requested `?card=` straight back
   * out — clicking a claude row in the CARDS panel did nothing at all, while
   * the terminal row beside it opened the board.
   */
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
