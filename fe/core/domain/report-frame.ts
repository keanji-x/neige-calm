// The height of a report block drawn in a fixed box (`app`, `preview`, `window`, `html`): the
// kernel bounds an authored `height` to 120–2000 px, and each renderer picks its own default.

export const REPORT_FRAME_MIN_HEIGHT = 120;
export const REPORT_FRAME_MAX_HEIGHT = 2000;

/** An authored height clamped to the kernel's bounds; absent or `NaN` takes the renderer's default. */
export function reportFrameHeight(height: number | null | undefined, fallback: number): number {
  if (height == null || Number.isNaN(height)) return fallback;
  return Math.min(REPORT_FRAME_MAX_HEIGHT, Math.max(REPORT_FRAME_MIN_HEIGHT, height));
}
