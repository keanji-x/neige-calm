import type { RefObject } from 'react';
import { liftProjection } from './depth.ts';

export type Point = Readonly<{ x: number; y: number }>;
/** A primitive-owned host outside press feedback, never a focus/pointer lookup. */
export type SurfaceSource = Readonly<{ ref: RefObject<HTMLElement | null> }>;
export type LiftOrigin =
  | Readonly<{ kind: 'anchor'; anchor: SurfaceSource; unavailable: 'surface-center' }>
  | Readonly<{ kind: 'surface-center' }>
  | Readonly<{ kind: 'paired-edge'; edge: 'inline-start' | 'inline-end' }>;
export type SurfaceGeometry =
  | Readonly<{ kind: 'lift'; origin: LiftOrigin }>
  | Readonly<{ kind: 'edge-reveal'; origin: Extract<LiftOrigin, { kind: 'paired-edge' }> }>;
/** Snapshot of the actual paint map, including the primary camera reference. */
export type SurfaceMap = Readonly<{ kind: 'lift' }> | Readonly<{ kind: 'edge-reveal'; center: Point }>;
export type SurfaceFrame = Readonly<{
  document: Document;
  size: Readonly<{ width: number; height: number }>;
  viewportFromLocal: Readonly<{ x: number; y: number; scaleX: number; scaleY: number }>;
}>;
export type SurfacePart = Readonly<{ element: RefObject<HTMLElement | null>; readFrame: () => SurfaceFrame | null }>;
export type SurfaceParts =
  | Readonly<{ kind: 'single'; primary: SurfacePart }>
  | Readonly<{ kind: 'paired'; primary: SurfacePart; peer: SurfacePart }>;
export type LiftState = Readonly<{ progress: number; x: number; y: number }>;
export type LiftSample = Readonly<{ value: LiftState; velocity: LiftState }>;

export function validateFrame(frame: SurfaceFrame, document: Document) {
  const { size, viewportFromLocal: a } = frame;
  if (frame.document !== document) throw new Error('Surface frames must share a document');
  if (![size.width, size.height, a.x, a.y, a.scaleX, a.scaleY].every(Number.isFinite)
    || size.width <= 0 || size.height <= 0 || a.scaleX <= 0 || a.scaleY <= 0) throw new Error('Unsupported surface frame');
}

/** Reject unsupported mappings rather than interpreting a rotated bounding box as scale. */
export function assertAxisAligned(element: HTMLElement) {
  const view = element.ownerDocument.defaultView;
  if (view === null) throw new Error('Surface requires a window');
  for (let node: HTMLElement | null = element; node !== null; node = node.parentElement) {
    const css = view.getComputedStyle(node);
    const matrix = new view.DOMMatrixReadOnly(css.transform === 'none' ? undefined : css.transform);
    const scales = css.scale === 'none' || !css.scale ? [1] : css.scale.split(' ').map(Number);
    if (!matrix.is2D || matrix.b !== 0 || matrix.c !== 0 || matrix.a <= 0 || matrix.d <= 0
      || (css.perspective && css.perspective !== 'none')
      || (css.rotate && css.rotate !== 'none' && css.rotate !== '0deg')
      || scales.some(value => !Number.isFinite(value) || value <= 0)) throw new Error('Unsupported surface transform');
  }
}

/** Layout owner calls this on its unanimated border box, never on the painted part. */
export function readSurfaceFrame(positioner: HTMLElement | null): SurfaceFrame | null {
  if (positioner === null || !positioner.isConnected || positioner.getClientRects().length === 0) return null;
  assertAxisAligned(positioner);
  const rect = positioner.getBoundingClientRect();
  const css = positioner.ownerDocument.defaultView!.getComputedStyle(positioner);
  const extra = (axis: 'Width' | 'Height') => axis === 'Width'
    ? parseFloat(css.paddingLeft) + parseFloat(css.paddingRight) + parseFloat(css.borderLeftWidth) + parseFloat(css.borderRightWidth)
    : parseFloat(css.paddingTop) + parseFloat(css.paddingBottom) + parseFloat(css.borderTopWidth) + parseFloat(css.borderBottomWidth);
  const width = parseFloat(css.width) + (css.boxSizing === 'border-box' ? 0 : extra('Width'));
  const height = parseFloat(css.height) + (css.boxSizing === 'border-box' ? 0 : extra('Height'));
  if (!(width > 0 && height > 0 && rect.width > 0 && rect.height > 0)) return null;
  return { document: positioner.ownerDocument, size: { width, height },
    viewportFromLocal: { x: rect.left, y: rect.top, scaleX: rect.width / width, scaleY: rect.height / height } };
}

export function frameCenter(frame: SurfaceFrame): Point {
  const a = frame.viewportFromLocal;
  return { x: a.x + a.scaleX * frame.size.width / 2, y: a.y + a.scaleY * frame.size.height / 2 };
}

/** One displacement and analytical derivative for keyframes and rebase. */
function displacement(frame: SurfaceFrame, origin: Point, map: SurfaceMap, progress: number) {
  const { scale, sink, scaleDerivative, sinkDerivative } = liftProjection(progress);
  const center = map.kind === 'lift' ? frame.viewportFromLocal : map.center;
  return { scale: map.kind === 'lift' ? scale : 1,
    x: (1 - scale) * (origin.x - center.x), y: (1 - scale) * (origin.y - center.y) + sink,
    dx: -scaleDerivative * (origin.x - center.x), dy: -scaleDerivative * (origin.y - center.y) + sinkDerivative };
}

export function surfaceKeyframe(frame: SurfaceFrame, origin: Point, map: SurfaceMap, state: LiftState): Keyframe {
  const a = frame.viewportFromLocal;
  const d = displacement(frame, origin, map, state.progress);
  const x = (d.x + state.x) / a.scaleX; const y = (d.y + state.y) / a.scaleY;
  return { transformOrigin: '0px 0px', transform: `matrix(${d.scale}, 0, 0, ${d.scale}, ${x}, ${y})`,
    opacity: Math.max(0, Math.min(1, state.progress)) };
}
export function liftKeyframe(frame: SurfaceFrame, origin: Point, state: LiftState): Keyframe {
  return surfaceKeyframe(frame, origin, { kind: 'lift' }, state);
}

/** Preserve only the motion contribution in local space, allowing normal layout/scroll to move. */
export function rebaseSurface(state: LiftSample, old: SurfaceFrame, oldOrigin: Point, oldMap: SurfaceMap,
  next: SurfaceFrame, origin: Point, map: SurfaceMap): LiftSample {
  const before = displacement(old, oldOrigin, oldMap, state.value.progress);
  const after = displacement(next, origin, map, state.value.progress);
  const axis = (key: 'x' | 'y', derivative: 'dx' | 'dy', ratio: number) => ({
    value: ratio * (before[key] + state.value[key]) - after[key],
    velocity: ratio * (before[derivative] * state.velocity.progress + state.velocity[key])
      - after[derivative] * state.velocity.progress,
  });
  const x = axis('x', 'dx', next.viewportFromLocal.scaleX / old.viewportFromLocal.scaleX);
  const y = axis('y', 'dy', next.viewportFromLocal.scaleY / old.viewportFromLocal.scaleY);
  return { value: { progress: state.value.progress, x: x.value, y: y.value },
    velocity: { progress: state.velocity.progress, x: x.velocity, y: y.velocity } };
}
export function rebaseLift(state: LiftSample, old: SurfaceFrame, oldOrigin: Point, next: SurfaceFrame, origin: Point): LiftSample {
  return rebaseSurface(state, old, oldOrigin, { kind: 'lift' }, next, origin, { kind: 'lift' });
}

export type BindingView = Readonly<{ lifetime: object; revision: number }> & (
  | Readonly<{ kind: 'unavailable' }>
  | Readonly<{ kind: 'ready'; parts: readonly Readonly<{ element: HTMLElement; frame: SurfaceFrame }>[] }>
);

/** Current declared binding facts; no descendant geometry or paint inversion. */
export type SurfaceBinding = Readonly<{
  readBinding: () => BindingView;
}>;
