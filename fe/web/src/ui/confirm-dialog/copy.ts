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

/**
 * INV-DUP-010, again — a card's delete is offered from two places at once (the
 * wave panel's CARDS row and the card's own head on the board), and both are
 * the same irreversible act on the same row, so they read the same sentence.
 *
 * The consequence names the runtime, not the row: what a reader stands to lose
 * by deleting a `codex` card is the session inside it, and "the card is
 * removed" would have described the cheapest half of that.
 */
export const DELETE_CARD_COPY = Object.freeze({
  title: 'Delete this card?',
  description: 'The card and anything running in it — its terminal or agent session — are removed. This cannot be undone.',
  confirmLabel: 'Delete card',
});

/**
 * CR-5 / CR-5a — parameterised, but still the single declaration site. INV-DUP-010
 * protects "one home", not "one string": both cove entry points (sidebar row, cove
 * page header) call this same function.
 *
 * Four fields, not three: §6.13's body is two sentences with different typography
 * (consequence at --text-base/--text, prompt at --text-xs/--text-3), and a single
 * `description` slot cannot carry both. The component owns the layout; this file
 * owns only the strings.
 */
export function deleteCoveCopy(coveName: string, waveCount: number | undefined) {
  return Object.freeze({
    title: `Delete ${coveName}?`,
    consequence: waveCount === undefined
      ? 'The number of waves is not available. Every wave in this cove will be deleted. This cannot be undone.'
      : waveCount === 0
      ? 'This deletes the cove. This cannot be undone.'
      : waveCount === 1
      ? 'This deletes 1 wave. This cannot be undone.'
      : `This deletes ${waveCount} waves. This cannot be undone.`,
    prompt: `Type ${coveName} to confirm.`,
    confirmLabel: 'Delete cove',
  });
}
