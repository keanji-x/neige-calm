import { describe, expect, it } from 'vitest';

import { createCardRegistry } from '../registry.js';
import { CODEX_CARD_ENTRY } from './codex.ts';
import { partitionWaveCards } from './headless-filter.js';
import { registerAvailableBuiltinCards } from './register.js';

function wire(id: string, kind: string, payload: unknown) {
  return {
    id, kind, wave_id: 'w1', title: null, sort: 0, payload,
    deletable: true, created_at: 0, updated_at: 0,
  };
}

describe('CODEX_CARD_ENTRY', () => {
  it('resolves kernel codex cards, including before terminal_id is projected', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x1', kind: 'codex', payload: { terminal_id: 't1' },
    })).toEqual({ type: 'codex', id: 'x1', title: null, terminalId: 't1' });
    // The kernel projects `terminal_id` on read; a card observed between mint
    // and projection resolves with a null terminal rather than not at all.
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x2', kind: 'codex', payload: { goal: 'do the thing' },
    })).toEqual({ type: 'codex', id: 'x2', title: null, terminalId: null });
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x3', kind: 'terminal', payload: { terminal_id: 't1' },
    })).toBeNull();
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 'x4', kind: 'claude', payload: { terminal_id: 't1' },
    })).toBeNull();
  });

  /*
   * `INV-CARD-180`. Kind `'codex'` mints two different cards; the payload's
   * `spec_harness` bit is the only discriminator. Codex is registered *before*
   * spec and takes no `claim`, so if it stopped refusing harness payloads it
   * would swallow every spec card into a surface-owning card — spec cards are
   * headless by `INV-CARD-181` and must not appear in the CARDS list at all.
   */
  it('[INV-CARD-180] refuses spec harness payloads so they fall through to spec', () => {
    expect(CODEX_CARD_ENTRY.fromKernel?.({
      id: 's1', kind: 'codex', payload: { spec_harness: true },
    })).toBeNull();
    // A refusal is only useful if the card really lands on spec afterwards, so
    // assert it through the booted production registry, in production order.
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(
      registry.resolve({ id: 's1', kind: 'codex', payload: { spec_harness: true } })?.type,
      'a spec harness card must still resolve as spec through the production registry',
    ).toBe('spec');
    expect(registry.get('spec')?.headless).toBe(true);
    // …and an ordinary codex card must not be taken by spec.
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't1' } })?.type)
      .toBe('codex');
  });

  it('reads only the exact discriminator, not any truthy spec_harness', () => {
    // `isSpecHarnessPayload` is `=== true`; sharing it with spec is what keeps
    // the two entries from disagreeing about which cards are harnesses.
    for (const payload of [{ spec_harness: false }, { spec_harness: 'true' }, { spec_harness: 1 }, null, 'x']) {
      expect(CODEX_CARD_ENTRY.fromKernel?.({ id: 'p', kind: 'codex', payload })?.type).toBe('codex');
    }
  });

  it('is kernel-minted-only — worker cards come from a task row, not a gesture', () => {
    expect(CODEX_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
  });

  it('registers as a surface-owning built-in', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.get('codex')?.headless).toBe(false);
    expect(registry.resolve({ id: 'x1', kind: 'codex', payload: { terminal_id: 't9' } })?.type)
      .toBe('codex');
  });

  /*
   * The bug this card exists to fix (#1150). A codex card with no adapter fell
   * into `unknown`, so `app/router` left it out of `gridItems`, `knownCard`
   * stayed false and the route effect replaced the requested `?card=` straight
   * back out — clicking a codex row in the CARDS panel did nothing at all,
   * while the terminal row beside it opened the board.
   */
  it('lands codex cards in the visible partition beside terminal cards', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    const { visible, unknown } = partitionWaveCards(registry, [
      wire('k-codex', 'codex', { terminal_id: 't1' }),
      wire('k-term', 'terminal', { terminal_id: 't2' }),
      // Same kernel kind, harness bit set: still headless, still filtered out.
      wire('k-spec', 'codex', { spec_harness: true }),
    ]);
    expect(visible.map((slot) => slot.card.type)).toEqual(['codex', 'terminal']);
    expect(unknown).toEqual([]);
  });
});
