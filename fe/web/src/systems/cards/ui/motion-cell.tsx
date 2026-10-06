import { forwardRef, useCallback, useEffectEvent, useLayoutEffect, useRef, type CSSProperties, type HTMLAttributes } from 'react';
import { transformStrategy } from 'react-grid-layout/core';
import type { Position } from 'react-grid-layout/core';
import { useState } from '../../../ui/state/public.ts';
import { playPointSpring, type PointPlayback, type SpringPoint } from '../../../ui/motion/spring.ts';

/** Publish vendor-owned pixel coordinates as a narrow renderer contract, not an inferred CSS matrix. */
export function motionPositionStyle(position: Position): CSSProperties {
  return { ...transformStrategy.calcStyle(position), '--nc-card-layout-x': String(position.left), '--nc-card-layout-y': String(position.top) } as CSSProperties;
}
function positionOf(style: CSSProperties | undefined): SpringPoint {
  const fields = style as Record<string, unknown> | undefined;
  const x = fields?.['--nc-card-layout-x'];
  const y = fields?.['--nc-card-layout-y'];
  if (typeof x !== 'string' || typeof y !== 'string' || !Number.isFinite(Number(x)) || !Number.isFinite(Number(y))) {
    throw new Error('Card motion requires declared grid coordinates');
  }
  return { x: Number(x), y: Number(y) };
}

export type Manipulation = 'drag' | 'resize' | null;

/** RGL still owns style, DOM handlers and layout. Motion owns only settled translation. */
export const MotionCell = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement> & { enabled: boolean; manipulation: Manipulation }>(function MotionCell({ enabled, manipulation, style, children, ...props }, externalRef) {
  const ref = useRef<HTMLDivElement | null>(null);
  const target = positionOf(style);
  const settled = useRef(target);
  const shift = useRef<SpringPoint>({ x: 0, y: 0 });
  const [offset, setOffset] = useState<SpringPoint>({ x: 0, y: 0 });
  const active = useRef<PointPlayback | null>(null);
  const assignRef = useCallback((element: HTMLDivElement | null) => {
    ref.current = element;
    if (typeof externalRef === 'function') externalRef(element);
    else if (externalRef !== null) externalRef.current = element;
  }, [externalRef]);
  const cancel = useEffectEvent(() => { active.current?.cancel(); active.current = null; });
  const clearShift = useEffectEvent(() => {
    if (shift.current.x === 0 && shift.current.y === 0) return;
    shift.current = { x: 0, y: 0 }; setOffset(shift.current);
  });
  const immediate = useEffectEvent((value: SpringPoint) => { cancel(); clearShift(); settled.current = value; });
  const reduce = useEffectEvent(() => { immediate(target); });
  const move = useEffectEvent((target: SpringPoint, enabled: boolean, manipulation: Manipulation) => {
    const element = ref.current;
    if (element === null) return;
    if (!enabled || manipulation === 'drag' || window.matchMedia('(prefers-reduced-motion: reduce)').matches) { immediate(target); return; }
    if (manipulation === 'resize') {
      // RGL resizes at the logical goal; retain the painted translation while dimensions follow the pointer.
      if (active.current !== null) {
        const painted = active.current.sample().value;
        shift.current = { x: painted.x - target.x, y: painted.y - target.y };
        setOffset(shift.current);
      }
      cancel();
      settled.current = { x: target.x + shift.current.x, y: target.y + shift.current.y };
      return;
    }
    const from = active.current?.sample() ?? { value: settled.current, velocity: { x: 0, y: 0 } };
    cancel(); clearShift();
    if (from.value.x === target.x && from.value.y === target.y && from.velocity.x === 0 && from.velocity.y === 0) return;
    const playback = playPointSpring([element], from.value, target, from.velocity,
      value => ({ transform: `translate(${value.x}px, ${value.y}px)` }));
    active.current = playback;
    void playback.finished.then(() => {
      if (active.current !== playback) return;
      settled.current = target; cancel();
    }, () => { /* Direct manipulation discarded this animation. */ });
  });
  // Pass render-owned targets explicitly: a vendor layout effect can update children before Effect Events refresh.
  useLayoutEffect(() => { move({ x: target.x, y: target.y }, enabled, manipulation); }, [target.x, target.y, enabled, manipulation]);
  useLayoutEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => { if (preference.matches) reduce(); };
    preference.addEventListener('change', change);
    return () => { preference.removeEventListener('change', change); cancel(); };
  }, []);
  const renderedStyle = offset.x === 0 && offset.y === 0 ? style : { ...style, transform: `translate(${target.x + offset.x}px, ${target.y + offset.y}px)` };
  return <div {...props} ref={assignRef} style={renderedStyle}>{children}</div>;
});
