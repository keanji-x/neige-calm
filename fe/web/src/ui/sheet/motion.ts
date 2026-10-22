import { playSpring, type SpringSample } from '../motion/spring.ts';
import { createPlaybackTarget } from '../motion/target.ts';
import { notifyAncestorVisualChange } from '../hover-preview/visual-change.ts';

/** One paint writer and playback identity per connected panel lifetime. Layout
 * changes retain q/v; q is motion relative to the declared unanimated frame. */
export function createSheetMotion(panel: HTMLDivElement) {
  const active = createPlaybackTarget<number>();
  let live = true;
  let revision = 0;
  // Translation leaves border-box height invariant in the declared unscaled
  // CSS-pixel frame. Keep fractional travel; offsetHeight would round it.
  let painted: SpringSample | null = null;
  const paint = (value: number) => ({ transform: `translateY(${value}px)` });
  const write = (value: number, announce = true) => {
    panel.style.transform = value === 0 ? '' : paint(value).transform;
    if (announce) notifyAncestorVisualChange(panel);
  };
  const hold = (): SpringSample => {
    const sample = active.sample() ?? painted ?? { value: panel.getBoundingClientRect().height, velocity: 0 };
    // Preserve paint before removing the native fill effect.
    write(sample.value, false); painted = sample;
    active.setOnVisualChange(undefined); active.cancel(); revision++;
    // Publish only after the old native fill can no longer override inline paint.
    notifyAncestorVisualChange(panel);
    return sample;
  };
  return {
    hold,
    direct: (sample: SpringSample) => { if (live) { painted = sample; write(sample.value); } },
    move: (to: number, immediate: boolean, settled: () => void) => {
      // Initialization is available only after native show/layout. A hidden
      // border box (including its default zero) is never ready paint. Existing
      // q/v remains untouched while geometry is unavailable.
      if (!live || !panel.isConnected || panel.getClientRects().length === 0) return;
      const height = panel.getBoundingClientRect().height;
      if (!Number.isFinite(height) || height <= 0 || getComputedStyle(panel).visibility !== 'visible') return;
      const from = hold(); const generation = revision;
      if (immediate || from.value === to && from.velocity === 0) {
        painted = { value: to, velocity: 0 }; write(to); settled(); return;
      }
      const playback = playSpring([panel], from.value, to, from.velocity, paint);
      active.replace(playback, () => {
        if (!live || revision !== generation || !panel.isConnected) return;
        painted = { value: to, velocity: 0 }; write(to); settled();
      });
      playback.setOnVisualChange(() => {
        if (live && revision === generation && panel.isConnected) notifyAncestorVisualChange(panel);
      });
    },
    dispose: () => { live = false; revision++; active.setOnVisualChange(undefined); active.cancel(); },
  };
}
export type SheetMotion = ReturnType<typeof createSheetMotion>;
