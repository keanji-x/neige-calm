// The claude worker card. Kernel kind `'claude'`. Owns a surface: Claude runs as a TUI inside a PTY, so this reuses `TerminalCardView` outright.
// `terminal_id` is not on the stored card row; the kernel projects it into the payload on read.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalSessionFromCard, cwdFromPayload, type TerminalCard } from './terminal.ts';

declare module '../registry.js' {
  interface CardDataMap {
    claude: ClaudeCard;
  }
}

export type ClaudeCard = Readonly<{
  type: 'claude';
  id: string;
  title: string | null;
  terminalId: string | null;
  sessionState: TerminalCard['sessionState'];
  cwd: string | null;
  gateCwd: string | null;
}>;

/** Lowercase to match the terminal card's own `'terminal'` fallback; `LetterAvatar` lowercases the title itself before picking the avatar colour. */
const CLAUDE_FALLBACK_TITLE = 'claude';

export const CLAUDE_CARD_ENTRY = Object.freeze({
  type: 'claude',
  component: (props: CardComponentProps<ClaudeCard>) => TerminalCardView({
    ...props,
    fallbackTitle: CLAUDE_FALLBACK_TITLE,
  }),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  claim: Object.freeze({ mode: 'exact', kind: 'claude' } as const),
  title: (card: ClaudeCard) => card.title ?? 'Claude',
  accessibleName: (card: ClaudeCard) => card.title ?? 'Claude',
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): ClaudeCard | null => (
    card.kind === 'claude'
      ? Object.freeze({
        type: 'claude',
        id: card.id,
        title: null,
        ...terminalSessionFromCard(card),
        cwd: cwdFromPayload(card.payload),
        gateCwd: cwdFromPayload(card.payload, 'gate_cwd'),
      } as const)
      : null
  ),
}) satisfies CardEntry<ClaudeCard>;
