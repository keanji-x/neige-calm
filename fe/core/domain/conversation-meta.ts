/** Timing evidence supplied by the owner; null explicitly means unmeasured. */
export type ConversationMetaClock = Readonly<{
  elapsedMs: number | null;
  timestamp: Readonly<{ kind: 'finished' | 'paused'; atMs: number }> | null;
}>;
