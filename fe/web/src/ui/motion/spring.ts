import { calcGeneratorDuration, spring } from 'motion';

// One shared response frequency. Critical damping and normalized mass are derived conventions.
const RESPONSE = 20;
// Rendering precision, independent of component distance or animation feel.
const SAMPLE_MS = 10;

export type SpringSample = Readonly<{ value: number; velocity: number }>;
export type SpringPoint = Readonly<{ x: number; y: number }>;
type Playback<Value> = Readonly<{
  animations: readonly Animation[];
  finished: Promise<void>;
  sample: () => Readonly<{ value: Value; velocity: Value }>;
  cancel: () => void;
}>;

export type SpringPlayback = Playback<number>;
export type PointPlayback = Playback<SpringPoint>;

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

/** Shared native renderer, independent of scalar/vector geometry. */
function playTrajectory<Value>(
  elements: readonly (HTMLElement | SVGElement)[],
  trajectory: Readonly<{ duration: number; sample: (time: number) => Readonly<{ value: Value; velocity: Value }> }>,
  paint: (value: Value) => Keyframe,
): Playback<Value> {
  if (elements.length === 0) throw new Error('Spring requires a surface');
  const document = elements[0].ownerDocument;
  if (elements.some(element => element.ownerDocument !== document)) throw new Error('Spring surfaces must share a document');
  const steps = Math.max(1, Math.ceil(trajectory.duration / SAMPLE_MS));
  const keyframes = Array.from({ length: steps + 1 }, (_, index) => paint(trajectory.sample(trajectory.duration * index / steps).value));
  const animations: Animation[] = [];
  try {
    for (const element of elements) animations.push(element.animate(keyframes, {
      duration: trajectory.duration, easing: 'linear', fill: 'both',
    }));
  } catch (error) {
    for (const animation of animations) { void animation.finished.catch(() => {}); animation.cancel(); }
    throw error;
  }
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

export function playSpring(
  elements: readonly (HTMLElement | SVGElement)[], from: number, to: number, velocity: number,
  paint: (value: number) => Keyframe,
): SpringPlayback {
  return playTrajectory(elements, springTrajectory(from, to, velocity), paint);
}

export function playPointSpring(
  elements: readonly (HTMLElement | SVGElement)[], from: SpringPoint, to: SpringPoint, velocity: SpringPoint,
  paint: (value: SpringPoint) => Keyframe,
): PointPlayback {
  const x = springTrajectory(from.x, to.x, velocity.x);
  const y = springTrajectory(from.y, to.y, velocity.y);
  return playTrajectory(elements, {
    duration: Math.max(x.duration, y.duration),
    sample: time => {
      const a = x.sample(Math.min(time, x.duration));
      const b = y.sample(Math.min(time, y.duration));
      return { value: { x: a.value, y: b.value }, velocity: { x: a.velocity, y: b.velocity } };
    },
  }, paint);
}
