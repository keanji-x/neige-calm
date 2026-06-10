import type { WaveCardSlot } from '../types';

export interface WorkerCardSlot {
  slot: WaveCardSlot;
  originalIndex: number;
}

/** Filter spec + wave-report cards out of a wave's card list while
 *  preserving the original index so callers like onRemoveCard can
 *  still address the underlying `detail.cards[idx]`. */
export function excludeReportCards(cards: WaveCardSlot[]): WorkerCardSlot[] {
  return cards
    .map((slot, originalIndex) => ({ slot, originalIndex }))
    .filter(({ slot }) => {
      if (slot.kind === 'card') {
        return slot.card.type !== 'spec' && slot.card.type !== 'wave-report';
      }
      // unknown kernel kinds: filter wave-report by raw kernel kind too
      // (defensive; adapter should have caught it, but unknown is a fallback).
      // spec kernel kind is 'codex' with spec_harness flag, so it cannot
      // be identified here without payload access.
      return slot.kernelKind !== 'wave-report';
    });
}
