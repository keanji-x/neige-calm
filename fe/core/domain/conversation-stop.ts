/** Transient stop feedback is not a persisted terminal outcome. */
export type ConversationStopFeedback = Readonly<
  | { kind: 'requesting' }
  | { kind: 'stopping' }
  | { kind: 'unconfirmed' }
  | { kind: 'failed'; message: string }
>;
