// The claude worker card. Kernel kind `'claude'`. Owns a surface.
//
// Claude runs as an interactive TUI inside a PTY — `sh -c "claude …"` spawned
// by `ClaudeWorkerAdapter` — so the card *is* a terminal card wearing a
// different name. That is why this reuses `TerminalCardView` outright instead
// of growing a parallel renderer: there is one PTY surface in this system and
// a second copy of it would drift.
//
// `terminal_id` is not on the stored card row; the kernel projects it into the
// payload on read (`routes/cards.rs::project_runtime_into_cards_payload`), the
// same way terminal cards get theirs. Reading it from the payload therefore
// needs no wire-schema change — and the shared reader below is the reason a
// future move of that field cannot fix one card kind while silently breaking
// the other.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { TerminalCardView } from './terminal-card.tsx';
import { terminalIdFromPayload } from './terminal.js';

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
}>;

/**
 * Lowercase on purpose. `LetterAvatar` keys its `card-head-icon--claude`
 * semantic colour off the head title lowercased, and the terminal card's own
 * fallback is likewise lowercase `'terminal'` — so this is the existing
 * convention, not a stray style choice.
 */
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
  // Worker cards are minted by the kernel dispatcher off a task row; there is
  // no user-facing "new claude card" gesture to model here.
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
  fromKernel: (card: KernelCardInput): ClaudeCard | null => (
    card.kind === 'claude'
      ? Object.freeze({
        type: 'claude',
        id: card.id,
        title: null,
        terminalId: terminalIdFromPayload(card.payload),
      } as const)
      : null
  ),
}) satisfies CardEntry<ClaudeCard>;
