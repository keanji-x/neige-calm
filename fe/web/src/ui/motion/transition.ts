/** Semantic recipes shared by CSS and imperative Motion consumers. */
const RECIPES = Object.freeze({
  enter: Object.freeze({ duration: '--motion-medium', ease: '--ease-enter' }),
  exit: Object.freeze({ duration: '--motion-snappy', ease: '--ease-exit' }),
  layout: Object.freeze({ duration: '--motion-medium', ease: '--ease-layout' }),
  size: Object.freeze({ duration: '--motion-medium', ease: '--ease-emphasis' }),
  disclosure: Object.freeze({ duration: '--motion-snappy', ease: '--ease-layout' }),
  feedback: Object.freeze({ duration: '--motion-quick', ease: '--ease-feedback' }),
  emphasis: Object.freeze({ duration: '--motion-slow', ease: '--ease-emphasis' }),
});
export type MotionIntent = keyof typeof RECIPES;
export type MotionTransition = Readonly<{ duration: number; ease: [number, number, number, number] }>;

/** Read the owning surface's tokens; keep missing or invalid configuration explicit. */
export function readMotionTransition(element: Element, intent: MotionIntent): MotionTransition {
  const style = getComputedStyle(element);
  const { duration: durationToken, ease: easeToken } = RECIPES[intent];
  const duration = style.getPropertyValue(durationToken).trim().match(/^(\d*\.?\d+)(ms|s)$/);
  const curve = style.getPropertyValue(easeToken).trim().match(/^cubic-bezier\(([^)]+)\)$/);
  const coordinates = curve?.[1].split(',').map(value =>
    /^[+-]?(?:\d*\.?\d+)(?:e[+-]?\d+)?$/i.test(value.trim()) ? Number(value) : NaN);
  if (duration === null || coordinates?.length !== 4
    || coordinates.some(value => !Number.isFinite(value))
    || coordinates[0] < 0 || coordinates[0] > 1 || coordinates[2] < 0 || coordinates[2] > 1) {
    throw new Error(`Invalid motion tokens: ${durationToken}, ${easeToken}`);
  }
  const seconds = Number(duration[1]) / (duration[2] === 'ms' ? 1000 : 1);
  if (!Number.isFinite(seconds)) throw new Error(`Invalid motion token: ${durationToken}`);
  return { duration: seconds, ease: [coordinates[0], coordinates[1], coordinates[2], coordinates[3]] };
}

/** Intrinsic-size travel: reference 128px, bounded to 65–150% of the surface's base duration. */
export function readSizeTransition(element: Element, from: number, to: number): MotionTransition {
  if (!Number.isFinite(from) || !Number.isFinite(to) || from < 0 || to < 0) {
    throw new Error('Invalid intrinsic motion size');
  }
  const transition = readMotionTransition(element, 'size');
  const travel = Math.abs(to - from);
  const factor = Math.min(1.5, Math.max(.65, Math.sqrt(travel / 128)));
  return { ...transition, duration: travel === 0 ? 0 : transition.duration * factor };
}
