import { useEffect } from 'react';
import { useState } from '../../../ui/state/public.ts';

/**
 * Milliseconds since `startMs`, re-rendered every second while it is set; `null` when there is no
 * start to measure from. The interval runs only while `startMs` is set and is cleared on unmount.
 */
export function useRunningElapsedMs(startMs: number | null): number | null {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (startMs === null) return;
    const timer = setInterval(() => setTick((tick) => tick + 1), 1000);
    return () => clearInterval(timer);
  }, [startMs]);
  return startMs === null ? null : Date.now() - startMs;
}
