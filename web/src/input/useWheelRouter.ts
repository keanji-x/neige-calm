import { useEffect, type RefObject } from 'react';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
} from './wheelRouter';

export function useWheelRouter(scrollRef: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const scrollRoot = scrollRef.current;
    if (!scrollRoot) return;

    const handleWheel = (event: WheelEvent) => {
      const activeCard = getActiveCardShell(
        scrollRoot,
        document,
        event.clientX,
        event.clientY,
      );
      const route = resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: event.target,
        deltaY: event.deltaY,
        deltaMode: event.deltaMode,
      });

      if (route.kind === 'page' || route.kind === 'xterm-passthrough') return;

      if (route.kind === 'native-scroll') {
        event.preventDefault();
        const { x, y } = pixelDelta(event);
        route.target.scrollLeft += x;
        route.target.scrollTop += y;
        return;
      }
      if (route.kind === 'xterm-scrollback') {
        if (route.target.scrollback(event.deltaY, event.deltaMode)) {
          event.preventDefault();
        }
        return;
      }
    };

    scrollRoot.addEventListener('wheel', handleWheel, {
      capture: true,
      passive: false,
    });

    return () => {
      scrollRoot.removeEventListener('wheel', handleWheel, { capture: true });
    };
  }, [scrollRef]);
}
