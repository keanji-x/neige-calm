// Headless-card filtering (`INV-CARD-226`).
//
// A wave's card list arrives as `CardWire[]` and is addressed by position:
// remove/action callbacks reach back into `detail.cards[index]`. So the index
// must be stamped on **before** anything is filtered out — a post-filter
// display index addresses a different card and deletes the wrong one.
//
// Two kinds disappear from the list once resolved: `spec` and `wave-report` are
// headless by contract. The unknown branch additionally drops raw kind
// `'wave-report'` defensively, because an unknown slot is a fallback and a
// report card must never surface as a diagnosable panel. Spec has no equivalent
// defence: its kernel kind is `'codex'` and without a readable payload it
// cannot be recognised here. That gap is known and accepted by `INV-CARD-226`.

import type { CardWire } from '../../../../../core/domain/wave.ts';
import type { CardRegistry, RegisteredCard } from '../registry.js';

/** A card with a real adapter that owns a surface. */
export type VisibleCardSlot = Readonly<{
  card: RegisteredCard;
  wire: CardWire;
  originalIndex: number;
}>;

/** A card no registered adapter claimed; renders as a diagnostic slot. */
export type UnknownCardSlot = Readonly<{
  wire: CardWire;
  originalIndex: number;
}>;

export type WaveCardPartition = Readonly<{
  visible: readonly VisibleCardSlot[];
  unknown: readonly UnknownCardSlot[];
}>;

/** Card types that resolve successfully but never occupy a slot. */
export const HEADLESS_CARD_TYPES = Object.freeze(['spec', 'wave-report'] as const);

function isHeadlessType(type: string): boolean {
  return (HEADLESS_CARD_TYPES as readonly string[]).includes(type);
}

/**
 * Split a wave's cards into surface-bearing and unknown slots.
 *
 * Order is left exactly as it arrived — sorting by `CardWire.sort` belongs to
 * the grid, not here — but `originalIndex` is carried out of both branches so
 * that whoever sorts later can still address the original wire.
 */
export function partitionWaveCards(
  registry: CardRegistry,
  cards: readonly CardWire[],
): WaveCardPartition {
  const visible: VisibleCardSlot[] = [];
  const unknown: UnknownCardSlot[] = [];
  // `originalIndex` is bound here, over the unfiltered array, and never recomputed.
  cards.forEach((wire, originalIndex) => {
    const card = registry.resolve({ id: wire.id, kind: wire.kind, payload: wire.payload });
    if (card === null) {
      if (wire.kind !== 'wave-report') unknown.push(Object.freeze({ wire, originalIndex }));
      return;
    }
    if (isHeadlessType(card.type)) return;
    visible.push(Object.freeze({ card, wire, originalIndex }));
  });
  return Object.freeze({ visible: Object.freeze(visible), unknown: Object.freeze(unknown) });
}
