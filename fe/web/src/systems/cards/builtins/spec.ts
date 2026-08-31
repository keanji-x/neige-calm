import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    spec: SpecCard;
  }
}

export type SpecCard = Readonly<{ type: 'spec'; id: string }>;

export function isSpecHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { spec_harness?: unknown }).spec_harness === true;
}

export const SPEC_CARD_ENTRY = Object.freeze({
  type: 'spec',
  component: () => null,
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
