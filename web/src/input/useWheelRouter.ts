import { useEffect, type RefObject } from 'react';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
} from './wheelRouter';

const MODAL_SELECTOR = '.modal-overlay, .modal-panel';
const WHEEL_CARD_SELECTOR = '[data-wheel-card]';

function asElement(target: EventTarget | null): Element | null {
  return target instanceof Element ? target : null;
}

export function useWheelRouter(scrollRef: RefObject<HTMLElement | null>): void {
  useEffect(() => {
    const scrollRoot = scrollRef.current;
    if (!scrollRoot) return;

    const handleWheel = (event: WheelEvent) => {
      const activeCard = getActiveCardShell(scrollRoot, document);
      const route = resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: event.target,
        deltaY: event.deltaY,
      });

      if (route.kind === 'page' || route.kind === 'xterm-passthrough') return;

      event.preventDefault();
      if (route.kind === 'native-scroll') {
        const { x, y } = pixelDelta(event);
        route.target.scrollLeft += x;
        route.target.scrollTop += y;
        return;
      }
      if (route.kind === 'xterm-scrollback') {
        route.target.scrollback(event.deltaY, event.deltaMode);
      }
    };

    const handlePointerDown = (event: PointerEvent) => {
      const target = asElement(event.target);
      if (!target || target.closest(MODAL_SELECTOR)) return;
      if (target.closest(WHEEL_CARD_SELECTOR)) return;

      const activeCard = getActiveCardShell(scrollRoot, document);
      const activeElement = document.activeElement;
      if (
        activeCard &&
        activeElement instanceof HTMLElement &&
        activeCard.contains(activeElement)
      ) {
        activeElement.blur();
      }
    };

    scrollRoot.addEventListener('wheel', handleWheel, {
      capture: true,
      passive: false,
    });
    scrollRoot.addEventListener('pointerdown', handlePointerDown, {
      capture: true,
    });

    return () => {
      scrollRoot.removeEventListener('wheel', handleWheel, { capture: true });
      scrollRoot.removeEventListener('pointerdown', handlePointerDown, {
        capture: true,
      });
    };
  }, [scrollRef]);
}
