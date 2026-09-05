// The cooked-shell PTY card. Kernel kind `'terminal'`. Owns a surface.

import type { CardComponentProps, CardEntry, KernelCardInput } from '../registry.js';
import type { WorkerSessionState } from '../../../../../core/api/schemas.js';
import { TerminalCardView } from './terminal-card.tsx';

declare module '../registry.js' {
  interface CardDataMap {
    terminal: TerminalCard;
  }
}

export type TerminalCard = Readonly<{
  type: 'terminal';
  id: string;
  title: string | null;
  terminalId: string | null;
  sessionState: WorkerSessionState | null;
  cwd: string | null;
  gateCwd: string | null;
}>;

/** Runtime identity wins even when it has no PTY; payload is legacy-only. */
export function terminalSessionFromCard(card: KernelCardInput): Pick<TerminalCard, 'terminalId' | 'sessionState'> {
  return {
    terminalId: card.runtime === undefined
      ? terminalIdFromPayload(card.payload)
      : card.runtime.terminal_id || null,
    sessionState: card.runtime?.status ?? null,
  };
}

/**
 * Shared with `claude.ts` and `codex.ts`: all three kinds get `terminal_id`
 * projected into the payload by the same kind-agnostic kernel read path
 * (`session_projection_lookup.rs::project_runtime_fields`), so all three must
 * read it the same way.
 */
export function terminalIdFromPayload(payload: unknown): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const value = (payload as { terminal_id?: unknown }).terminal_id;
  return typeof value === 'string' && value !== '' ? value : null;
}

export function cwdFromPayload(payload: unknown, key: 'cwd' | 'gate_cwd' = 'cwd'): string | null {
  if (typeof payload !== 'object' || payload === null) return null;
  const value = (payload as { cwd?: unknown; gate_cwd?: unknown })[key];
  return typeof value === 'string' && value.trim() !== '' ? value : null;
}

export const TERMINAL_CARD_ENTRY = Object.freeze({
  type: 'terminal',
  component: (props: CardComponentProps<TerminalCard>) => TerminalCardView(props),
  headless: false,
  defaultSize: Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 }),
  claim: Object.freeze({ mode: 'exact', kind: 'terminal' } as const),
  title: (card: TerminalCard) => card.title ?? 'Terminal',
  accessibleName: (card: TerminalCard) => card.title ?? 'Terminal',
  create: Object.freeze({
    mode: 'atomic' as const,
    submit: (): Promise<{ cardId: string }> => Promise.reject(new Error('TerminalCardSubmitViaTrackRoute')),
  }),
  /* No fields: a terminal has nothing to ask. It opens in the track's own
     directory, and naming it before it exists is a decision the reader has no
     information for yet — the head is renamable once there is something in it. */
  addPanel: Object.freeze({ label: 'terminal' }),
  fromKernel: (card: KernelCardInput): TerminalCard | null => (
    card.kind === 'terminal'
      ? Object.freeze({ type: 'terminal', id: card.id, title: null, ...terminalSessionFromCard(card), cwd: cwdFromPayload(card.payload), gateCwd: cwdFromPayload(card.payload, 'gate_cwd') } as const)
      : null
  ),
}) satisfies CardEntry<TerminalCard>;
