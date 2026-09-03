import type { TrackCardSlot } from '../types';

export interface WorkerCardSlot {
  slot: TrackCardSlot;
  originalIndex: number;
}

/** Filter spec + track-report cards out of a track's card list while
 *  preserving the original index so callers like onRemoveCard can
 *  still address the underlying `detail.cards[idx]`. */
export function excludeReportCards(cards: TrackCardSlot[]): WorkerCardSlot[] {
  return cards
    .map((slot, originalIndex) => ({ slot, originalIndex }))
    .filter(({ slot }) => {
      if (slot.kind === 'card') {
        return slot.card.type !== 'spec' && slot.card.type !== 'track-report';
      }
      // unknown kernel kinds: filter track-report by raw kernel kind too
      // (defensive; adapter should have caught it, but unknown is a fallback).
      // spec kernel kind is 'codex' with spec_harness flag, so it cannot
      // be identified here without payload access.
      return slot.kernelKind !== 'track-report';
    });
}
