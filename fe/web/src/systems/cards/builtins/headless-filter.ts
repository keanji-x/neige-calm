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
     * Optional rather than required because the frozen interface tests
     * (`public.test.ts`, `public.contract.test.ts`) build entry literals
     * without it and register them for real; a required member here would fail
     * their typecheck, and neither file may be edited. Absent therefore means
     * "has a surface", which is the common case and the safe default for the
     * unknown slot.
     *
     * Built-ins do not get to rely on that default: `builtins/register.ts`
     * registers them through a factory that requires the member, and its
     * registrar map's value type is that factory's nominal result — so a
     * missing declaration is a typecheck error there, as is any *ordinary*
     * structural stand-in for the factory. That gate is not airtight and does
     * not claim to be: an explicit assertion, and an open set of runtime
     * object-construction APIs that need no assertion at all, still fill a map
     * slot, and several of them run rather than throwing at boot. See the
     * `BuiltinRegistrar` doc comment in `builtins/register.ts` for the escapes
     * that are known today and for what actually holds the line — a per-entry
     * runtime `typeof entry.headless === 'boolean'` assertion over the real
     * production registry. A mis-declared `true` likewise still compiles and
     * deletes every card of that type, so the registry-wide contract test pins
     * both directions by type and separately requires the declaration to be a
     * boolean.
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
    // Back to the entry that minted this card, so the declaration is read off
    // the entry rather than a list kept beside it. The registry does not
    // guarantee this round trip: `resolve` returns whatever `fromKernel`
    // produced and never checks that its `type` is the entry's own, so an entry
    // returning a foreign type would send this lookup to `undefined` or to
    // another entry's metadata, and a headless card would fail open into
    // `visible`. What actually holds it is the entries: `CardEntry<Card>` ties
    // `type: Card['type']` to `fromKernel: (…) => Card | null`, so a production
    // entry written against the narrow generic cannot mismatch and compile.
    // `register.contract.test.ts` asserts the round trip on every registered
    // entry, because an entry annotated as a bare `CardEntry` would drop that
    // compile-time tie without any other signal.
    if (registry.get(card.type)?.headless === true) return;
    visible.push(Object.freeze({ card, wire, originalIndex }));
  });
  return Object.freeze({ visible: Object.freeze(visible), unknown: Object.freeze(unknown) });
}
