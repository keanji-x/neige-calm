import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    planner: PlannerCard;
  }
}

export type PlannerCard = Readonly<{ type: 'planner'; id: string }>;

export function isPlannerHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { planner_harness?: unknown }).planner_harness === true;
}

export const PLANNER_CARD_ENTRY = Object.freeze({
  type: 'planner',
  component: () => null,
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Planner',
  accessibleName: () => 'Planner harness',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): PlannerCard | null => (
    card.kind === 'codex' && isPlannerHarnessPayload(card.payload)
      ? Object.freeze({ type: 'planner', id: card.id } as const)
      : null
  ),
}) satisfies CardEntry<PlannerCard>;
