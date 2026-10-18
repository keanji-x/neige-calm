/** UI-owned camera model: rest is zero; positive depth recedes into the page. */
const CAMERA = 800;
const PRESS_DEPTH = 28;
const TEXT_SURFACE_DEPTH = 8;
const SINK_PER_DEPTH = 0.07;
// An initial press carries an inward impulse, without jumping position or delaying click.
export const TAP_VELOCITY = 16;

export function depthFrame(depth: number): Keyframe {
  return { transform: `translateY(${depth * SINK_PER_DEPTH}px) scale(${CAMERA / (CAMERA + depth)})` };
}

export function pressDepthFrame(progress: number): Keyframe {
  return { ...depthFrame(progress * PRESS_DEPTH), composite: 'add' };
}

/** Text surfaces travel less than action controls, but share camera and direction. */
export function presenceDepthFrame(progress: number): Keyframe {
  return { ...depthFrame((1 - progress) * TEXT_SURFACE_DEPTH), opacity: Math.max(0, Math.min(1, progress)) };
}
