import type { PlannerRunningTurn } from './conversation.js';

/** Timing evidence supplied by the owner; null explicitly means unmeasured. */
export type ConversationMetaClock = Readonly<{
  elapsedMs: number | null;
  timestamp: Readonly<{ kind: 'finished' | 'paused'; atMs: number }> | null;
}>;

/** Where a running turn's clock starts, on this client's clock. */
export type RunningTurnAnchor = Readonly<{ turnId: string; startMs: number }>;

/**
 * Fold one run response into the running clock's anchor: `start = receivedAtMs - elapsed_ms`.
 * The same turn keeps the EARLIER anchor (returning `previous` itself), so a refetch whose network
 * delay shifts the estimate later never moves the clock backwards; a new turn re-anchors and `null`
 * clears it.
 */
export function anchorRunningTurn(
  previous: RunningTurnAnchor | null, running: PlannerRunningTurn | null, receivedAtMs: number,
): RunningTurnAnchor | null {
  if (running === null) return null;
  const startMs = receivedAtMs - running.elapsed_ms;
  if (previous !== null && previous.turnId === running.turn_id && previous.startMs <= startMs) return previous;
  return { turnId: running.turn_id, startMs };
}
