import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { isAssistantHarnessPayload } from './assistant.ts';
import { isPlannerHarnessPayload } from './planner.ts';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalSessionFromCard, cwdFromPayload, type TerminalCard } from './terminal.ts';

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
  sessionState: TerminalCard['sessionState'];
  cwd: string | null;
  gateCwd: string | null;
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
  /* `atomic`: `POST /api/tracks/:id/codex-cards` writes the row and spawns the daemon in one call. The submit is not implemented here: this module has no transport and may not acquire one; `app/router` owns the call. */
  create: Object.freeze({
    mode: 'atomic' as const,
    submit: (): Promise<{ cardId: string }> => Promise.reject(new Error('CodexCardSubmitViaTrackRoute')),
  }),
  addPanel: Object.freeze({
    label: 'codex',
    fields: Object.freeze([
      Object.freeze({ key: 'title', label: 'Title', kind: 'text' as const, placeholder: 'Codex' }),
      Object.freeze({
        key: 'cwd',
        label: 'Working directory',
        kind: 'directory' as const,
        hint: "Optional. Left empty, codex runs in the track's own directory.",
      }),
    ]),
  }),
  fromKernel: (card: KernelCardInput): CodexCard | null => (
    card.kind === 'codex'
      && !isPlannerHarnessPayload(card.payload)
      && !isPlainChatPayload(card.payload)
      /* `codex` is scanned before `assistant`, so without this clause this entry would claim every track-assistant card and the headless `ASSISTANT_CARD_ENTRY` would never be reached. */
      && !isAssistantHarnessPayload(card.payload)
      ? Object.freeze({
        type: 'codex',
        id: card.id,
        title: null,
        ...terminalSessionFromCard(card),
        cwd: cwdFromPayload(card.payload),
        gateCwd: cwdFromPayload(card.payload, 'gate_cwd'),
      } as const)
      : null
  ),
}) satisfies CardEntry<CodexCard>;
