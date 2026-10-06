import { useRef, type ReactNode } from 'react';
import { useSpringTarget } from './target.ts';

/** Decorative rotation; the owning disclosure declares its orientation and accessible state. */
export function SpringRotation({ angle, className, children }: Readonly<{
  angle: number; className: string; children: ReactNode;
}>) {
  const ref = useRef<HTMLSpanElement | null>(null);
  useSpringTarget(ref, null, angle, angle, true, true, () => {}, value => ({ rotate: `${value}deg` }));
  return <span ref={ref} className={className} aria-hidden="true" data-nc-spring-rotation="" style={{ rotate: `${angle}deg` }}>{children}</span>;
}
