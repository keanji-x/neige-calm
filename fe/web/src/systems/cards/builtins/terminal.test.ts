import { describe, expect, it } from 'vitest';

import { createCardRegistry } from '../registry.js';
import { registerAvailableBuiltinCards } from './register.js';
import { TERMINAL_CARD_ENTRY } from './terminal.ts';

describe('TERMINAL_CARD_ENTRY', () => {
  it('resolves kernel terminal cards, including an empty payload before terminal_id lands', () => {
    expect(TERMINAL_CARD_ENTRY.fromKernel?.({
      id: 'c1', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toEqual({ type: 'terminal', id: 'c1', title: null, terminalId: 't1', sessionState: null });
    expect(TERMINAL_CARD_ENTRY.fromKernel?.({
      id: 'c2', kind: 'terminal', payload: {},
    })).toEqual({ type: 'terminal', id: 'c2', title: null, terminalId: null, sessionState: null });
    expect(TERMINAL_CARD_ENTRY.fromKernel?.({
      id: 'c3', kind: 'codex', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  it('uses the web TrackGrid terminal default size', () => {
    expect(TERMINAL_CARD_ENTRY.defaultSize).toEqual({ w: 6, h: 10, minW: 4, minH: 6 });
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const entry = registry.get('terminal');
    expect(entry?.headless).toBe(false);
    expect(registry.resolve({ id: 'c1', kind: 'terminal', payload: { terminal_id: 't9' } })?.type)
      .toBe('terminal');
  });
});
