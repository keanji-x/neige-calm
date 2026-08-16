// Headless-card filtering (`INV-CARD-226`).
//
// A wave's card list arrives as `CardWire[]` and is addressed by position:
// remove/action callbacks reach back into `detail.cards[index]`. So the index
// must be stamped on **before** anything is filtered out — a post-filter
// display index addresses a different card and deletes the wrong one.
//
// Whether a card is headless is the entry's own declaration, not a list kept
// beside it: `CardEntry.headless` is augmented onto the entry interface below
// and each built-in states its own answer next to its `component` and
// `defaultSize`. A parallel type-name list would be a second place to forget —
// and forgetting either way deletes cards from the product.
//
// Today `spec` and `wave-report` declare it. The unknown branch additionally
// drops raw kind `'wave-report'` defensively, because an unknown slot is a
// fallback and a report card must never surface as a diagnosable panel. Spec
// has no equivalent defence: its kernel kind is `'codex'` and without a
// readable payload it cannot be recognised here. That gap is known and accepted
// by `INV-CARD-226`.

import type { CardWire } from '../../../../../core/domain/wave.ts';
import type { CardRegistry, RegisteredCard } from '../registry.js';

declare module '../registry.js' {
  interface CardEntry {
    /**
     * The card resolves, but owns no surface: it never occupies a slot in the
     * CARDS list or the grid (`INV-CARD-226`).
     *
     * Optional rather than required because `public.contract.test.ts` is a
     * frozen file whose entry literals are checked with `satisfies CardEntry`;
     * a required member would fail its typecheck. Absent therefore means "has a
     * surface", which is the common case and the safe default for the unknown
     * slot — a mis-declared `true` deletes every card of that type, so the
     * registry-wide contract test pins both directions.
     */
    readonly headless?: boolean;
  }
}

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
    // `resolve` returned this card, so the entry exists; `get` is the way back
    // to it. Reading the declaration off the entry is what keeps this in step
    // with whatever entries are actually registered.
    if (registry.get(card.type)?.headless === true) return;
    visible.push(Object.freeze({ card, wire, originalIndex }));
  });
  return Object.freeze({ visible: Object.freeze(visible), unknown: Object.freeze(unknown) });
}
