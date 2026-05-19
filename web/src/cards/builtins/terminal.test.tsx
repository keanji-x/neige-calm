// Unit tests for the terminal card registry entry's `fromKernel` adapter.
//
// We deliberately don't render the actual TerminalCard component here — it
// pulls in `@xterm/xterm` which needs a real canvas, and our concern is the
// kernel→UI adapter contract (the discriminator + payload parse), not the
// xterm wiring. Render tests for live PTYs belong in playwright e2e.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { KernelCard } from '../../api/wire';

// Stub `XtermView` (and its xterm/css imports) so importing this card module
// doesn't pull a real terminal into jsdom.
vi.mock('../../XtermView', () => ({
  XtermView: () => null,
}));

import { TerminalEntry } from './terminal';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'card_1',
    wave_id: 'wave_1',
    kind: 'terminal',
    sort: 0,
    payload: { terminal_id: 'term_42' },
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

describe('TerminalEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('returns a typed terminal card for a valid payload', () => {
    const k = makeKernelCard();
    const card = TerminalEntry.fromKernel!(k);
    expect(card).not.toBeNull();
    expect(card!.type).toBe('terminal');
    expect(card!.id).toBe('card_1');
    expect(card!.terminalId).toBe('term_42');
    expect(card!.lines).toEqual([]);
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('accepts a card with a null payload (predates terminal_id attach)', () => {
    // The adapter treats `null` as "empty object" — kernel will patch the
    // payload in later. The card renders in static mode meanwhile.
    const k = makeKernelCard({ payload: null });
    const card = TerminalEntry.fromKernel!(k);
    expect(card).not.toBeNull();
    expect(card!.terminalId).toBeUndefined();
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it("returns null when kind doesn't match", () => {
    const k = makeKernelCard({ kind: 'doc' });
    const card = TerminalEntry.fromKernel!(k);
    expect(card).toBeNull();
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('returns null and warns when payload fails schema parse', () => {
    // Non-object payload on a terminal-kind card is the documented error
    // path — the schema requires `{ terminal_id?: string }`.
    const k = makeKernelCard({ payload: 'not-an-object' });
    const card = TerminalEntry.fromKernel!(k);
    expect(card).toBeNull();
    expect(warnSpy).toHaveBeenCalledTimes(1);
    expect(String(warnSpy.mock.calls[0]![0])).toContain('terminal payload invalid');
  });
});
