import type { CardWire } from '../../../../../core/domain/track.ts';
import type { CardRegistry, RegisteredCard } from '../registry.js';

declare module '../registry.js' {
  interface CardEntry {
    /** Whether the card owns a visible surface. */
    readonly headless?: boolean;
  }
}

export type VisibleCardSlot = Readonly<{
  card: RegisteredCard;
  wire: CardWire;
  originalIndex: number;
}>;

export type UnknownCardSlot = Readonly<{
  wire: CardWire;
  originalIndex: number;
}>;

export type TrackCardPartition = Readonly<{
  visible: readonly VisibleCardSlot[];
  unknown: readonly UnknownCardSlot[];
}>;

export function partitionTrackCards(
  registry: CardRegistry,
  cards: readonly CardWire[],
): TrackCardPartition {
  const visible: VisibleCardSlot[] = [];
  const unknown: UnknownCardSlot[] = [];
  cards.forEach((wire, originalIndex) => {
    const card = registry.resolve(wire);
    if (card === null) {
      if (wire.kind !== 'track-report') unknown.push(Object.freeze({ wire, originalIndex }));
      return;
    }
    if (registry.get(card.type)?.headless === true) return;
    visible.push(Object.freeze({ card, wire, originalIndex }));
  });
  return Object.freeze({ visible: Object.freeze(visible), unknown: Object.freeze(unknown) });
}
