/**
 * INV-DUP-010 — the destructive-confirm copy, declared once.
 *
 * Delete affordances live in three places (sidebar row, cove page, wave page)
 * and a user must read the same sentence in all three; a wave delete is not
 * recoverable from the UI. Keeping the strings here is what stops one surface
 * from drifting into a softer wording than the others.
 */
export const DELETE_WAVE_COPY = Object.freeze({
  title: 'Delete this wave?',
  description: 'The wave, its cards, and their terminals are removed. This cannot be undone.',
  confirmLabel: 'Delete wave',
});

export const DELETE_COVE_COPY = Object.freeze({
  title: 'Delete this cove?',
  description: 'The cove and every wave inside it are removed. This cannot be undone.',
  confirmLabel: 'Delete cove',
});
