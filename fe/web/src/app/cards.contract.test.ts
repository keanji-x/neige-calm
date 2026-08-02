import { describe, expect, it } from 'vitest';

import type { CardEntry, CardRegistry } from '../systems/cards/public.js';
import { registerBuiltinCards } from './cards.js';

describe('card boot composition contract', () => {
  it('[INV-CARD-225] registers built-ins in the semantic fallback order', () => {
    const seen: string[] = [];
    const registry: CardRegistry = {
      register: (entry) => { seen.push(entry.type); },
      get: () => undefined,
      resolve: () => null,
      entries: () => [],
    };
    const make = (type: string) => ({ type }) as CardEntry;
    registerBuiltinCards(
      registry,
      make('terminal'),
      make('codex'),
      make('spec'),
      make('claude'),
      make('wave-report'),
      make('file-viewer'),
      make('iframe'),
      make('plugin-iframe'),
    );
    expect(seen).toEqual([
      'terminal',
      'codex',
      'spec',
      'claude',
      'wave-report',
      'file-viewer',
      'iframe',
      'plugin-iframe',
    ]);
  });
});
