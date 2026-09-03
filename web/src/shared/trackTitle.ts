// Issue #409 — centralize the display fallback for tracks created without a title.

export const UNTITLED_TRACK_LABEL = 'Untitled track';

export function trackDisplayTitle(title: string): string {
  return title.trim() || UNTITLED_TRACK_LABEL;
}
