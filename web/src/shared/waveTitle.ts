// Issue #409 — centralize the display fallback for waves created without a title.

export const UNTITLED_WAVE_LABEL = 'Untitled wave';

export function waveDisplayTitle(title: string): string {
  return title.trim() || UNTITLED_WAVE_LABEL;
}
