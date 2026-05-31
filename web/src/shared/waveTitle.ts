export const UNTITLED_WAVE_LABEL = 'Untitled wave';

export function waveDisplayTitle(title: string): string {
  return title.trim() || UNTITLED_WAVE_LABEL;
}
