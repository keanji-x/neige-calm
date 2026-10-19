import { useCallback, useLayoutEffect, useMemo, useRef } from 'react';
import type { ScrollInput } from '../../../ui/drawer/follow-scroll.ts';

export type HistoryPrefetch = Readonly<{
  active: boolean;
  cursor: number | null;
  busy: boolean;
  failed: boolean;
  load: () => boolean;
}>;

/** A visit owns at most one unspent input, for its current Query cursor. Neither
 * page completion nor geometry creates input. Repeated input coalesces while busy.
 * A short inactivity lease lets newest reads finish without polling. */
export function useHistoryPrefetch(history: HistoryPrefetch | undefined): (input: ScrollInput) => void {
  const cursor = history?.cursor;
  const active = history?.active;
  const owner = useMemo(() => ({ cursor, active, current: null as {
    input: (input: ScrollInput) => void;
    resume: () => void;
  } | null }), [cursor, active]);
  const latest = useRef(history);
  useLayoutEffect(() => { latest.current = history; });
  useLayoutEffect(() => {
    let alive = true;
    let pending = false;
    let spent = false;
    let expiry: ReturnType<typeof setTimeout> | undefined;
    const cancel = () => {
      pending = false;
      clearTimeout(expiry);
    };
    // Network admission needs intent and an idle Query, not a paint/idle slot.
    // Only a busy newest read may defer this cursor's bounded authorization.
    const resume = () => {
      const state = latest.current;
      if (!alive || !pending || state?.active !== true || document.visibilityState === 'hidden' || state.cursor !== cursor || state.failed) return cancel();
      if (state.busy) return;
      if (state.load()) { pending = false; spent = true; clearTimeout(expiry); }
    };
    const visibility = () => { if (document.visibilityState === 'hidden') cancel(); };
    document.addEventListener('visibilitychange', visibility);
    owner.current = {
      resume,
      input(input) {
        if (!alive) return;
        if (input.direction !== 'up' || input.top > input.height
          || (input.source === 'keyboard' && !['PageUp', 'Home', 'ArrowUp'].includes(input.key))) return cancel();
        const state = latest.current;
        if (spent || document.visibilityState === 'hidden' || state?.active !== true || state.failed || cursor == null) return;
        pending = true;
        clearTimeout(expiry);
        expiry = setTimeout(cancel, 250);
        resume();
      },
    };
    return () => { alive = false; cancel(); document.removeEventListener('visibilitychange', visibility); owner.current = null; };
  }, [cursor, active, owner]);
  useLayoutEffect(() => {
    if (history?.failed) owner.current?.input({ source: 'return', direction: null, top: 0, height: 0 });
    else owner.current?.resume();
  }, [history?.busy, history?.failed, owner]);
  return useCallback(input => owner.current?.input(input), [owner]);
}
