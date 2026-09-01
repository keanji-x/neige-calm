import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
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
  /*
   * `atomic`, not `kernel-minted-only`: `POST /api/waves/:id/codex-cards` writes
   * the row and spawns the daemon in one call, so there is exactly one way for
   * this card to come into existence and the UI may use it. It was
   * kernel-minted-only only for as long as the front-end had no create path at
   * all — the endpoint predates that restriction.
   *
   * The submit itself is not implemented here for the same reason
   * `TERMINAL_CARD_ENTRY`'s is not: this module has no transport and may not
   * acquire one (`systems/**` sits below `app/**`). `app/router` owns the call;
   * this strategy declares only that the kind is user-creatable.
   */
  create: Object.freeze({
    mode: 'atomic' as const,
    submit: (): Promise<{ cardId: string }> => Promise.reject(new Error('CodexCardSubmitViaWaveRoute')),
  }),
  /*
   * Two fields, and neither is codex's own configuration: an interactive codex
   * picks its model and its permission mode inside its own slash-command UX, so
   * asking here would be a second place to answer the same question — and the
   * one that cannot be changed afterwards.
   */
  addPanel: Object.freeze({
    label: 'codex',
    fields: Object.freeze([
      Object.freeze({ key: 'title', label: 'Title', kind: 'text' as const, placeholder: 'Codex' }),
      Object.freeze({
        key: 'cwd',
        label: 'Working directory',
        kind: 'directory' as const,
        hint: "Optional. Left empty, codex runs in the wave's own directory.",
      }),
    ]),
  }),
  fromKernel: (card: KernelCardInput): CodexCard | null => (
    card.kind === 'codex'
      && !isSpecHarnessPayload(card.payload)
      && !isPlainChatPayload(card.payload)
      ? Object.freeze({
        type: 'codex',
        id: card.id,
        title: null,
        terminalId: terminalIdFromPayload(card.payload),
      } as const)
      : null
  ),
}) satisfies CardEntry<CodexCard>;
