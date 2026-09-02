import type { CardEntry, KernelCardInput } from '../registry.js';

declare module '../registry.js' {
  interface CardDataMap {
    assistant: AssistantCard;
  }
}

export type AssistantCard = Readonly<{ type: 'assistant'; id: string }>;

/**
 * The wave-assistant discriminator (#1189), and the *only* copy of it.
 *
 * The kernel writes `harness_profile: "assistant"` onto the card it mints for a
 * wave conversation (`crates/calm-server/src/plain_chat.rs::card_is_wave_assistant`)
 * and that marker is what every question about the card is answered from: it is
 * hidden from CARDS because it has no surface, and it opens the conversation
 * drawer instead. Those are one decision, so the router imports this predicate
 * rather than re-spelling the payload check — the same arrangement `spec` has.
 *
 * It must not be widened to "any `harness_profile` marker": a cove chat card
 * carries `plain_chat` under the same field, and answering the assistant
 * question with it would give a cove chat the assistant's surface. The server
 * refuses to conflate them for the same reason and says so at length.
 */
export function isAssistantHarnessPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'assistant';
}

/**
 * Headless, exactly like `SPEC_CARD_ENTRY`: an assistant conversation is read
 * in the drawer the conversation panel opens, and a card that draws nothing
 * would otherwise sit in CARDS and on the board occupying an empty slot.
 *
 * Registering it is only half the job. `BUILTIN_CARD_ORDER` puts `codex` before
 * this entry, and `resolve` takes the first entry whose `fromKernel` answers —
 * so `CODEX_CARD_ENTRY` (headless: false) has to refuse this payload explicitly,
 * which it does. Without that refusal this entry is never reached and every
 * assistant conversation appears in CARDS as a terminal-shaped card.
 */
export const ASSISTANT_CARD_ENTRY = Object.freeze({
  type: 'assistant',
  component: () => null,
  headless: true,
  defaultSize: Object.freeze({ w: 1, h: 1, minW: 1, minH: 1 }),
  title: () => 'Assistant',
  accessibleName: () => 'Wave assistant',
  create: Object.freeze({ mode: 'kernel-minted-only' } as const),
  fromKernel: (card: KernelCardInput): AssistantCard | null => (
    card.kind === 'codex' && isAssistantHarnessPayload(card.payload)
      ? Object.freeze({ type: 'assistant', id: card.id } as const)
      : null
  ),
}) satisfies CardEntry<AssistantCard>;
