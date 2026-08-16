// The spec harness card (`INV-CARD-181`, `INV-CARD-182`).
//
// The kernel mints spec cards under kind `'codex'`; the *only* thing that tells
// a spec harness apart from an ordinary codex agent is the `spec_harness`
// discriminator on the payload. Widening this predicate to `kind === 'codex'`
// would swallow every ordinary codex card into a headless card that renders
// nothing and is filtered out of the wave's CARDS list — the card would simply
// vanish from the product. `spec.test.ts` holds that line.

import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    spec: SpecCard;
  }
}

export type SpecCard = Readonly<{ type: 'spec'; id: string }>;

/**
 * `INV-CARD-182` — the discriminator, and nothing else. Non-object and `null`
 * payloads are ordinary shapes on the wire, so they narrow to `false` instead
 * of throwing.
 */
export function isSpecHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { spec_harness?: unknown }).spec_harness === true;
}

/**
 * `INV-CARD-181` — headless: no component, 1x1, kernel-minted only.
 *
 * Deliberately carries no `claim`: the kernel kind `'codex'` is shared with the
 * (not yet landed) codex entry, so spec must stay on the registry's
 * insertion-ordered full scan rather than take an exact claim on the kind.
 */
export const SPEC_CARD_ENTRY = Object.freeze({
  type: 'spec',
  component: () => null,
  // The declaration `partitionWaveCards` reads. Dropping it puts a card that
  // renders nothing into the CARDS list and the grid.
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Spec',
  accessibleName: () => 'Spec harness',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): SpecCard | null => (
    card.kind === 'codex' && isSpecHarnessPayload(card.payload)
      ? Object.freeze({ type: 'spec', id: card.id } as const)
      : null
  ),
}) satisfies CardEntry<SpecCard>;
