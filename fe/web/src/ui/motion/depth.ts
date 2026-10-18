/** UI-owned camera model: rest is zero; positive depth recedes into the page. */
const CAMERA = 800;
const PRESS_DEPTH = 28;
const TEXT_SURFACE_DEPTH = 8;
const SINK_PER_DEPTH = 0.07;
// An initial press carries an inward impulse, without jumping position or delaying click.
export const TAP_VELOCITY = 16;

export function depthProjection(depth: number) {
  return { scale: CAMERA / (CAMERA + depth), sink: depth * SINK_PER_DEPTH,
    scaleDerivative: -CAMERA / (CAMERA + depth) ** 2, sinkDerivative: SINK_PER_DEPTH };
}

export function depthFrame(depth: number): Keyframe {
  const { scale, sink } = depthProjection(depth);
  return { transform: `translateY(${sink}px) scale(${scale})` };
}

export function liftProjection(progress: number) {
  const projection = depthProjection((1 - progress) * TEXT_SURFACE_DEPTH);
  return { scale: projection.scale, sink: projection.sink,
    scaleDerivative: -TEXT_SURFACE_DEPTH * projection.scaleDerivative,
    sinkDerivative: -TEXT_SURFACE_DEPTH * projection.sinkDerivative };
}

export function pressDepthFrame(progress: number): Keyframe {
  return { ...depthFrame(progress * PRESS_DEPTH), composite: 'add' };
}

/** Text surfaces travel less than action controls, but share camera and direction. */
export function presenceDepthFrame(progress: number): Keyframe {
  return { ...depthFrame((1 - progress) * TEXT_SURFACE_DEPTH), opacity: Math.max(0, Math.min(1, progress)) };
}
