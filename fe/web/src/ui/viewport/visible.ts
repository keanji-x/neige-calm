import { useEffect } from 'react';
import { useState } from '../state/public.ts';

export type VisibleViewport = Readonly<{ height: number; bottomInset: number; bottomEdge: number }>;

/** Geometry shared by surfaces that keep controls anchored above the visible viewport's bottom. */
export function useVisibleViewport(enabled: boolean): VisibleViewport {
  const read = (): VisibleViewport => {
    const viewport = window.visualViewport;
    return viewport === null || viewport === undefined
      ? { height: window.innerHeight, bottomInset: 0, bottomEdge: window.innerHeight }
      : { height: viewport.height, bottomInset: Math.max(0, window.innerHeight - viewport.height - viewport.offsetTop), bottomEdge: viewport.height + viewport.offsetTop };
  };
  const [visible, setVisible] = useState(read);
  useEffect(() => {
    if (!enabled) return;
    const viewport = window.visualViewport;
    const sync = () => {
      const next = read();
      setVisible((previous) => previous.height === next.height && previous.bottomInset === next.bottomInset && previous.bottomEdge === next.bottomEdge ? previous : next);
    };
    sync();
    viewport?.addEventListener('resize', sync);
    viewport?.addEventListener('scroll', sync);
    window.addEventListener('resize', sync);
    return () => {
      viewport?.removeEventListener('resize', sync);
      viewport?.removeEventListener('scroll', sync);
      window.removeEventListener('resize', sync);
    };
  }, [enabled]);
  return visible;
}
