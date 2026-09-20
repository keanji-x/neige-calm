import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    assistant: AssistantCard;
  }
}

export type AssistantCard = Readonly<{ type: 'assistant'; id: string }>;

/** The track-assistant discriminator, and the only copy of it: the kernel writes `harness_profile: "assistant"` onto the card it mints. Must not be widened to any `harness_profile` marker — an area chat card carries `plain_chat` under the same field. */
export function isAssistantHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'assistant';
}

/** Headless, like `PLANNER_CARD_ENTRY`. `BUILTIN_CARD_ORDER` puts `codex` before this entry and `resolve` takes the first `fromKernel` that answers, so `CODEX_CARD_ENTRY` must refuse this payload explicitly. */
export const ASSISTANT_CARD_ENTRY = Object.freeze({
  type: 'assistant',
  component: () => null,
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Assistant',
  accessibleName: () => 'Track assistant',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): AssistantCard | null => (
    card.kind === 'codex' && isAssistantHarnessPayload(card.payload)
      ? Object.freeze({ type: 'assistant', id: card.id } as const)
      : null
  ),
}) satisfies CardEntry<AssistantCard>;
