import { useEffect, type RefObject } from 'react';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
} from './wheelRouter';
import { useCardEntryResolverRegistry } from '../cards/resolver';

export function useWheelRouter(scrollRef: RefObject<HTMLElement | null>): void {
  const resolveCardById = useCardEntryResolverRegistry();

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
        resolveCardById,
      });

      if (route.kind === 'page') return;

      if (route.kind === 'native-scroll') {
        event.preventDefault();
        const { x, y } = pixelDelta(event);
        route.target.scrollLeft += x;
        route.target.scrollTop += y;
        return;
      }
      if (route.kind === 'xterm') {
        const decision = route.target.decide(event.deltaY, event.deltaMode);
        if (decision.kind === 'pass') return;
        event.preventDefault();
        route.target.apply(event.deltaY, event.deltaMode);
        return;
      }
      if (route.kind === 'sink') {
        event.preventDefault();
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
  }, [resolveCardById, scrollRef]);
}
