/** Input owns following intent. Scroll/resize report geometry, including native
 * anchoring and programmatic writes. Navigation and restoration are explicit. */
export type ScrollFollower = Readonly<{
  attach(pane: HTMLElement, content: HTMLElement): () => void;
  followToEnd(): void;
  followGrowth(): void;
  navigate(position: () => void): void;
}>;

const RESTORATION_EVENT = 'nc-scroll-restoration';

/** Let a position owner lay out and restore several panes as one synchronous
 * operation. A follower waits until all captured reading places are restored. */
export function withScrollRestoration<T>(panes: readonly HTMLElement[], restore: () => T): T {
  for (const pane of panes) pane.dispatchEvent(new CustomEvent(RESTORATION_EVENT, { detail: true }));
  try { return restore(); }
  finally {
    for (const pane of panes) pane.dispatchEvent(new CustomEvent(RESTORATION_EVENT, { detail: false }));
  }
}

type Direction = 'up' | 'down';

/** A nested scrollport consumes its input before this pane, unless it has run
 * out of room and permits scroll chaining. Widgets may cancel keyboard defaults. */
function reachesPane(event: Event, pane: HTMLElement, direction: Direction): boolean {
  if (event.defaultPrevented) return false;
  for (const node of event.composedPath()) {
    if (node === pane) return true;
    if (!(node instanceof HTMLElement)) continue;
    const style = getComputedStyle(node);
    if (!['auto', 'scroll', 'overlay'].includes(style.overflowY)) continue;
    const room = direction === 'up' ? node.scrollTop : node.scrollHeight - node.clientHeight - node.scrollTop;
    if (room > 0 || ['contain', 'none'].includes(style.overscrollBehaviorY)) return false;
  }
  return false;
}

/** Domain-free: the caller owns the return-to-end tolerance and any navigation
 * targets. No message identities, transcript rules or global registry live here. */
export function createScrollFollower(options: Readonly<{
  bottomSlack: number;
  onAwayChange: (away: boolean) => void;
}>): ScrollFollower {
  let pane: HTMLElement | null = null;
  let following = true;
  let restoring = 0;
  let gesture: { top: number; direction?: Direction } | null = null;
  const distance = () => pane === null ? 0 : pane.scrollHeight - pane.scrollTop - pane.clientHeight;
  const measure = () => { options.onAwayChange(distance() > options.bottomSlack); };
  const followGrowth = () => {
    if (pane !== null && following && gesture === null && restoring === 0 && distance() > 1) {
      pane.scrollTop = pane.scrollHeight;
    }
    measure();
  };
  return {
    followGrowth,
    followToEnd() {
      gesture = null;
      following = true;
      if (pane !== null && restoring === 0) pane.scrollTop = pane.scrollHeight;
      measure();
    },
    navigate(position) {
      gesture = null;
      following = false;
      position();
      measure();
    },
    attach(target, content) {
      pane = target;
      following = true;
      restoring = 0;
      let attached = true;
      let pointer: number | null = null;
      let touchY: number | null = null;
      let touchHeld = false;
      const frames = new Set<number>();
      const frame = (callback: () => void) => {
        const id = requestAnimationFrame(() => { frames.delete(id); if (attached) callback(); });
        frames.add(id);
      };
      const endGesture = () => {
        const ended = gesture;
        if (ended === null) return;
        const top = ended.top;
        // Native scrolling can continue after scrollend from an older gesture.
        // Keep input provenance until two paints report no continuation.
        frame(() => frame(() => {
          if (gesture !== ended || ended.top !== top || pointer !== null || touchHeld) return;
          following ||= ended.direction === 'down' && distance() <= options.bottomSlack;
          gesture = null;
          followGrowth();
        }));
      };
      const begin = (event: Event, direction: Direction) => {
        if (!reachesPane(event, target, direction)) return;
        gesture = { top: gesture?.top ?? target.scrollTop, direction };
        const room = direction === 'up' ? target.scrollTop : distance();
        if (room > 0 || (direction === 'up' && distance() > options.bottomSlack)) following = false;
        if (room <= 0) endGesture();
      };
      const onWheel = (event: WheelEvent) => {
        if (event.ctrlKey || event.metaKey || event.deltaY === 0) return;
        begin(event, event.deltaY < 0 ? 'up' : 'down');
      };
      const onKey = (event: KeyboardEvent) => {
        const origin = event.target;
        if (!(origin instanceof HTMLElement) || origin.isContentEditable
          || origin.closest('input, textarea, select') !== null || event.isComposing || event.altKey || event.metaKey) return;
        if (event.key === ' ' && origin.closest('button, summary, [role="button"]') !== null) return;
        if (event.ctrlKey && event.key !== 'Home' && event.key !== 'End') return;
        if (['ArrowUp', 'PageUp', 'Home'].includes(event.key) || (event.key === ' ' && event.shiftKey)) begin(event, 'up');
        else if (['ArrowDown', 'PageDown', 'End', ' '].includes(event.key)) begin(event, 'down');
      };
      const onPointerDown = (event: PointerEvent) => {
        if (event.defaultPrevented || event.button !== 0 || event.pointerType === 'touch' || event.target !== target) return;
        pointer = event.pointerId;
        gesture = { top: target.scrollTop };
      };
      const onPointerMove = (event: PointerEvent) => {
        if (pointer === event.pointerId) gesture ??= { top: target.scrollTop };
      };
      const onPointerUp = (event: PointerEvent) => {
        if (pointer === null || pointer !== event.pointerId) return;
        pointer = null;
        endGesture();
      };
      const onTouchStart = (event: TouchEvent) => {
        touchHeld = true;
        touchY = event.touches.length === 1 ? event.touches[0].clientY : null;
      };
      const onTouchMove = (event: TouchEvent) => {
        const y = event.touches.length === 1 ? event.touches[0].clientY : null;
        if (y !== null && touchY !== null && y !== touchY) begin(event, y < touchY ? 'down' : 'up');
        touchY = y;
      };
      const onTouchEnd = (event: TouchEvent) => {
        touchY = null;
        if (event.touches.length > 0) return;
        touchHeld = false;
        endGesture();
      };
      const onScroll = () => {
        if (gesture !== null && restoring === 0) {
          const delta = target.scrollTop - gesture.top;
          gesture.top = target.scrollTop;
          if (delta !== 0) {
            const direction = delta < 0 ? 'up' : 'down';
            if (pointer !== null || gesture.direction === undefined || gesture.direction === direction) {
              gesture.direction = direction;
              following = false;
            }
          }
        }
        measure();
      };
      const onRestoration = (event: Event) => {
        if (!(event instanceof CustomEvent)) return;
        // Restored geometry is not movement from a pending input operation.
        gesture = null;
        restoring += event.detail === true ? 1 : -1;
        if (restoring === 0) followGrowth();
      };
      target.addEventListener('wheel', onWheel, { passive: true });
      target.ownerDocument.addEventListener('keydown', onKey);
      target.addEventListener('pointerdown', onPointerDown);
      target.addEventListener('pointermove', onPointerMove, { passive: true });
      target.ownerDocument.addEventListener('pointerup', onPointerUp);
      target.ownerDocument.addEventListener('pointercancel', onPointerUp);
      target.addEventListener('touchstart', onTouchStart, { passive: true });
      target.addEventListener('touchmove', onTouchMove, { passive: true });
      target.addEventListener('touchend', onTouchEnd);
      target.addEventListener('touchcancel', onTouchEnd);
      target.addEventListener('scroll', onScroll, { passive: true });
      target.addEventListener('scrollend', endGesture);
      target.addEventListener(RESTORATION_EVENT, onRestoration);
      const observer = new ResizeObserver(followGrowth);
      observer.observe(target);
      observer.observe(content);
      // Attach follows once, including a pane whose first content just arrived.
      target.scrollTop = target.scrollHeight;
      measure();
      return () => {
        attached = false;
        for (const id of frames) cancelAnimationFrame(id);
        observer.disconnect();
        target.removeEventListener('wheel', onWheel);
        target.ownerDocument.removeEventListener('keydown', onKey);
        target.removeEventListener('pointerdown', onPointerDown);
        target.removeEventListener('pointermove', onPointerMove);
        target.ownerDocument.removeEventListener('pointerup', onPointerUp);
        target.ownerDocument.removeEventListener('pointercancel', onPointerUp);
        target.removeEventListener('touchstart', onTouchStart);
        target.removeEventListener('touchmove', onTouchMove);
        target.removeEventListener('touchend', onTouchEnd);
        target.removeEventListener('touchcancel', onTouchEnd);
        target.removeEventListener('scroll', onScroll);
        target.removeEventListener('scrollend', endGesture);
        target.removeEventListener(RESTORATION_EVENT, onRestoration);
        if (pane === target) { pane = null; gesture = null; }
      };
    },
  };
}
