// The Claude card. Kernel kind `'claude'`. Owns a surface: Claude runs as a TUI inside a PTY, so `ClaudeCardView` reuses `TerminalCardView`.
// `terminal_id` is not on the stored card row; the kernel projects it into the payload on read.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import { ClaudeCardView, type ClaudeCard } from './claude-card.tsx';
import { terminalSessionFromCard, cwdFromPayload } from './terminal.ts';

export type { ClaudeCard };

declare module '../registry.js' {
  interface CardDataMap {
    claude: ClaudeCard;
  }
}

/** The kernel's creation-time marker of an owner-created Claude card (`OWNER_CREATED_PAYLOAD_KEY`). */
function ownerCreatedFromPayload(payload: unknown): boolean {
  return typeof payload === 'object' && payload !== null
    && (payload as { owner_created?: unknown }).owner_created === true;
}

export const CLAUDE_CARD_ENTRY = Object.freeze({
  type: 'claude',
  component: (props: CardComponentProps<ClaudeCard>) => ClaudeCardView(props),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  claim: Object.freeze({ mode: 'exact', kind: 'claude' } as const),
  title: (card: ClaudeCard) => card.title ?? 'Claude',
  accessibleName: (card: ClaudeCard) => card.title ?? 'Claude',
  /* `atomic`: `POST /api/tracks/:id/claude-cards` writes the row and spawns Claude in the track's workspace in one call
     (#2516). The submit is not implemented here: this module has no transport and may not acquire one; `app/router`
     owns the call, as for terminal and Codex cards. */
  create: Object.freeze({
    mode: 'atomic' as const,
    submit: (): Promise<{ cardId: string }> => Promise.reject(new Error('ClaudeCardSubmitViaTrackRoute')),
  }),
  addPanel: Object.freeze({ label: 'claude' }),
  fromKernel: (card: KernelCardInput): ClaudeCard | null => (
    card.kind === 'claude'
      ? Object.freeze({
        type: 'claude',
        id: card.id,
        title: null,
        ...terminalSessionFromCard(card),
        cwd: cwdFromPayload(card.payload),
        gateCwd: cwdFromPayload(card.payload, 'gate_cwd'),
        ownerCreated: ownerCreatedFromPayload(card.payload),
        workerSessionId: card.runtime?.worker_session_id ?? null,
        runningVersion: card.runtime?.claude_version ?? null,
      } as const)
      : null
  ),
}) satisfies CardEntry<ClaudeCard>;
