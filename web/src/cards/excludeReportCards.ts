import type { TrackCardSlot } from '../types';

export interface WorkerCardSlot {
  slot: TrackCardSlot;
  originalIndex: number;
}

/** Filter planner + track-report cards out of a track's card list while
 *  preserving the original index so callers like onRemoveCard can
 *  still address the underlying `detail.cards[idx]`. */
export function excludeReportCards(cards: TrackCardSlot[]): WorkerCardSlot[] {
  return cards
    .map((slot, originalIndex) => ({ slot, originalIndex }))
    .filter(({ slot }) => {
      if (slot.kind === 'card') {
        return slot.card.type !== 'planner' && slot.card.type !== 'track-report';
      }
      // unknown kernel kinds: filter track-report by raw kernel kind too
      // (defensive; adapter should have caught it, but unknown is a fallback).
      // planner kernel kind is 'codex' with planner_harness flag, so it cannot
      // be identified here without payload access.
      return slot.kernelKind !== 'track-report';
    });
}
