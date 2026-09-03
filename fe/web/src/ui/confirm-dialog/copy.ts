/**
 * INV-DUP-010 — the destructive-confirm copy, declared once.
 *
 * Delete affordances live in three places (sidebar row, area page, track page)
 * and a user must read the same sentence in all three; a track delete is not
 * recoverable from the UI. Keeping the strings here is what stops one surface
 * from drifting into a softer wording than the others.
 */
export const DELETE_TRACK_COPY = Object.freeze({
  title: 'Delete this track?',
  description: 'The track, its cards, and their terminals are removed. This cannot be undone.',
  confirmLabel: 'Delete track',
});

/**
 * INV-DUP-010, again — a card's delete is offered from two places at once (the
 * track panel's CARDS row and the card's own head on the board), and both are
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
 * protects "one home", not "one string": both area entry points (sidebar row, area
 * page header) call this same function.
 *
 * Four fields, not three: §6.13's body is two sentences with different typography
 * (consequence at --text-base/--text, prompt at --text-xs/--text-3), and a single
 * `description` slot cannot carry both. The component owns the layout; this file
 * owns only the strings.
 */
export function deleteAreaCopy(areaName: string, trackCount: number | undefined) {
  return Object.freeze({
    title: `Delete ${areaName}?`,
    consequence: trackCount === undefined
      ? 'The number of tracks is not available. Every track in this area will be deleted. This cannot be undone.'
      : trackCount === 0
      ? 'This deletes the area. This cannot be undone.'
      : trackCount === 1
      ? 'This deletes 1 track. This cannot be undone.'
      : `This deletes ${trackCount} tracks. This cannot be undone.`,
    prompt: `Type ${areaName} to confirm.`,
    confirmLabel: 'Delete area',
  });
}
