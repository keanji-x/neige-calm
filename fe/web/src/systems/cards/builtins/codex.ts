import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { isAssistantHarnessPayload } from './assistant.ts';
import { isSpecHarnessPayload } from './spec.ts';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalIdFromPayload } from './terminal.ts';

declare module '../registry.js' {
  interface CardDataMap {
    codex: CodexCard;
  }
}

export type CodexCard = Readonly<{
  type: 'codex';
  id: string;
  title: string | null;
  terminalId: string | null;
}>;

const CODEX_FALLBACK_TITLE = 'codex';

export function isPlainChatPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { harness_profile?: unknown }).harness_profile === 'plain_chat';
}

export const CODEX_CARD_ENTRY = Object.freeze({
  type: 'codex',
  component: (props: CardComponentProps<CodexCard>) => TerminalCardView({
    ...props,
    fallbackTitle: CODEX_FALLBACK_TITLE,
  }),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  title: (card: CodexCard) => card.title ?? 'Codex',
  accessibleName: (card: CodexCard) => card.title ?? 'Codex',
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): CodexCard | null => (
    card.kind === 'codex'
      && !isSpecHarnessPayload(card.payload)
      && !isPlainChatPayload(card.payload)
      /* #1189 — and the assistant marker, for the same reason as the two
         above: `codex` is scanned before `assistant`, so without this clause
         this entry claims every wave-assistant card and the headless
         `ASSISTANT_CARD_ENTRY` is never reached. The card would then appear in
         CARDS and on the board as an empty terminal. */
      && !isAssistantHarnessPayload(card.payload)
      ? Object.freeze({
        type: 'codex',
        id: card.id,
        title: null,
        terminalId: terminalIdFromPayload(card.payload),
      } as const)
      : null
  ),
}) satisfies CardEntry<CodexCard>;
