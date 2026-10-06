import { forwardRef, useCallback, useEffectEvent, useLayoutEffect, useRef, type CSSProperties, type HTMLAttributes } from 'react';
import { transformStrategy } from 'react-grid-layout/core';
import type { Position } from 'react-grid-layout/core';
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

/** RGL still owns style, DOM handlers and layout. Motion owns only settled translation. */
export const MotionCell = forwardRef<HTMLDivElement, HTMLAttributes<HTMLDivElement> & { enabled: boolean }>(function MotionCell({ enabled, style, children, ...props }, externalRef) {
  const ref = useRef<HTMLDivElement | null>(null);
  const target = positionOf(style);
  const settled = useRef(target);
  const active = useRef<PointPlayback | null>(null);
  const assignRef = useCallback((element: HTMLDivElement | null) => {
    ref.current = element;
    if (typeof externalRef === 'function') externalRef(element);
    else if (externalRef !== null) externalRef.current = element;
  }, [externalRef]);
  const cancel = useEffectEvent(() => { active.current?.cancel(); active.current = null; });
  const immediate = useEffectEvent(() => { cancel(); settled.current = target; });
  const move = useEffectEvent(() => {
    const element = ref.current;
    if (element === null) return;
    if (!enabled || window.matchMedia('(prefers-reduced-motion: reduce)').matches) { immediate(); return; }
    const from = active.current?.sample() ?? { value: settled.current, velocity: { x: 0, y: 0 } };
    cancel();
    if (from.value.x === target.x && from.value.y === target.y && from.velocity.x === 0 && from.velocity.y === 0) return;
    const playback = playPointSpring([element], from.value, target, from.velocity,
      value => ({ transform: `translate(${value.x}px, ${value.y}px)` }));
    active.current = playback;
    void playback.finished.then(() => {
      if (active.current !== playback) return;
      settled.current = target; cancel();
    }, () => { /* Direct manipulation discarded this animation. */ });
  });
  useLayoutEffect(() => { move(); }, [target.x, target.y, enabled]);
  useLayoutEffect(() => {
    const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
    const change = () => { if (preference.matches) immediate(); };
    preference.addEventListener('change', change);
    return () => { preference.removeEventListener('change', change); cancel(); };
  }, []);
  return <div {...props} ref={assignRef} style={style}>{children}</div>;
});
