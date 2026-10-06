import { createRef, useCallback, useLayoutEffect } from 'react';
import { useState } from './public.ts';

/** An event dispatcher for one view identity. Call only after commit, never from
 * render or a child layout effect. Abandoned renders cannot replace its target;
 * an old view's dispatcher keeps its own last committed target through A-B-A. */
export function useCommittedCallback<Args extends unknown[], Result>(
  viewId: string | null, callback: (...args: Args) => Result,
): (...args: Args) => Result {
  const [channel, setChannel] = useState(() => ({ viewId, target: createRef<typeof callback>() }));
  if (channel.viewId !== viewId) setChannel({ viewId, target: createRef<typeof callback>() });
  useLayoutEffect(() => { channel.target.current = callback; }, [channel, callback]);
  return useCallback((...args: Args) => {
    const target = channel.target.current;
    if (target === null) throw new Error('An event callback cannot run before its view commits.');
    return target(...args);
  }, [channel]);
}
