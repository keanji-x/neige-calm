import { calcGeneratorDuration, spring } from 'motion';

// One shared response frequency. Critical damping and normalized mass are derived conventions.
const RESPONSE = 20;
// Rendering precision, independent of component distance or animation feel.
const SAMPLE_MS = 10;

export type SpringSample = Readonly<{ value: number; velocity: number }>;
export type SpringPlayback = Readonly<{
  animations: readonly Animation[];
  finished: Promise<void>;
  sample: () => SpringSample;
  cancel: () => void;
}>;

/** Motion owns the equation, analytical velocity and stopping criteria. */
export function springTrajectory(from: number, to: number, velocity: number) {
  if (![from, to, velocity].every(Number.isFinite)) throw new Error('Invalid spring state');
  const generator = spring({ stiffness: RESPONSE ** 2, damping: 2 * RESPONSE, mass: 1, keyframes: [from, to], velocity });
  const resolveVelocity = generator.velocity;
  if (resolveVelocity === undefined) throw new Error('Motion spring has no velocity contract');
  const duration = calcGeneratorDuration(generator);
  if (!Number.isFinite(duration)) throw new Error('Motion spring did not converge');
  const sample = (time: number): SpringSample => {
    const state = generator.next(time);
    return { value: state.value, velocity: state.done ? 0 : resolveVelocity(time) };
  };
  return { duration, sample };
}

/** Adapt the shared library trajectory to owned native effects, without per-frame JS style writes. */
export function playSpring(
  elements: readonly HTMLElement[], from: number, to: number, velocity: number,
  paint: (value: number) => Keyframe,
): SpringPlayback {
  if (elements.length === 0) throw new Error('Spring requires a surface');
  const document = elements[0].ownerDocument;
  if (elements.some(element => element.ownerDocument !== document)) throw new Error('Spring surfaces must share a document');
  const trajectory = springTrajectory(from, to, velocity);
  const steps = Math.max(1, Math.ceil(trajectory.duration / SAMPLE_MS));
  const keyframes = Array.from({ length: steps + 1 }, (_, index) =>
    paint(trajectory.sample(trajectory.duration * index / steps).value));
  const animations: Animation[] = [];
  try {
    for (const element of elements) animations.push(element.animate(keyframes, {
      duration: trajectory.duration, easing: 'linear', fill: 'both',
    }));
  } catch (error) {
    for (const animation of animations) {
      void animation.finished.catch(() => {});
      animation.cancel();
    }
    throw error;
  }
  const start = document.timeline.currentTime;
  if (start !== null) for (const animation of animations) animation.startTime = start;
  return {
    animations,
    finished: Promise.all(animations.map(animation => animation.finished)).then(() => {}),
    sample: () => {
      const time = animations[0].currentTime;
      if (time !== null && typeof time !== 'number') throw new Error('Unsupported spring timeline');
      return trajectory.sample(Math.max(0, Math.min(trajectory.duration, time ?? 0)));
    },
    cancel: () => { for (const animation of animations) animation.cancel(); },
  };
}
