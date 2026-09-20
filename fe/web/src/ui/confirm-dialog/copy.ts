/** The destructive-confirm copy, declared once so every delete surface reads the same sentence. */
export const DELETE_TRACK_COPY = Object.freeze({
  title: 'Delete this track?',
  description: 'The track, its cards, and their terminals are removed. This cannot be undone.',
  confirmLabel: 'Delete track',
});

export const DELETE_CARD_COPY = Object.freeze({
  title: 'Delete this card?',
  description: 'The card and anything running in it — its terminal or agent session — are removed. This cannot be undone.',
  confirmLabel: 'Delete card',
});

/** Four fields, not three: consequence and prompt have different typography, so one `description` slot cannot carry both. */
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

export const RESET_TODAY_REPORT_COPY = Object.freeze({
  trigger: 'Reset',
  title: 'Reset today’s report?',
  description: 'Today’s report goes back to empty and what it says now is discarded. This cannot be undone. Conversations are not affected.',
  confirmLabel: 'Reset report',
});
