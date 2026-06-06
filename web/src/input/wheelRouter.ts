import type { XtermWheelTarget } from './xtermAdapter';
import { getXtermForShell } from './wheelTargets';
import type { CardWheelTargetDecl } from '../cards/lifecycle';
import { getEntry } from '../cards/registry';
import type { ResolveCardById } from '../cards/resolver';

export type WheelRoute =
  | { kind: 'page' }
  | { kind: 'native-scroll'; target: HTMLElement }
  | { kind: 'xterm'; target: XtermWheelTarget }
  | { kind: 'sink' };

const MODAL_SELECTOR = '.modal-overlay, .modal-panel';
const WHEEL_CARD_SELECTOR = '[data-wheel-card]';
const XTERM_ROOT_SELECTOR = '.xterm-view';
const LINE_HEIGHT_PX = 16;

function asElement(target: EventTarget | null): Element | null {
  return target instanceof Element ? target : null;
}

function closestWithin(
  start: Element | null,
  boundary: HTMLElement,
  predicate: (el: HTMLElement) => boolean,
): HTMLElement | null {
  let node: Element | null = start;
  while (node) {
    if (node !== boundary && !boundary.contains(node)) return null;
    if (node instanceof HTMLElement && predicate(node)) return node;
    if (node === boundary) break;
    node = node.parentElement;
  }
  return null;
}

function isScrollableY(el: HTMLElement): boolean {
  const overflowY = getComputedStyle(el).overflowY;
  return (
    (overflowY === 'auto' || overflowY === 'scroll') &&
    el.scrollHeight > el.clientHeight
  );
}

export function pixelDelta(event: WheelEvent): { x: number; y: number } {
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) {
    return {
      x: event.deltaX * LINE_HEIGHT_PX,
      y: event.deltaY * LINE_HEIGHT_PX,
    };
  }
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    const root =
      event.currentTarget instanceof HTMLElement
        ? event.currentTarget
        : document.documentElement;
    const page = root.clientHeight || 400;
    return { x: event.deltaX * page, y: event.deltaY * page };
  }
  return { x: event.deltaX, y: event.deltaY };
}

export function getActiveCardShell(
  scrollRoot: HTMLElement,
  ownerDocument: Document,
  clientX: number,
  clientY: number,
): HTMLElement | null {
  const el = ownerDocument.elementFromPoint(clientX, clientY);
  if (!el) return null;
  const shell = el.closest<HTMLElement>(WHEEL_CARD_SELECTOR);
  if (!shell) return null;
  return scrollRoot.contains(shell) ? shell : null;
}

function isXtermViewHandle(value: unknown): value is {
  getWheelTarget(): XtermWheelTarget | null;
} {
  return (
    value !== null &&
    typeof value === 'object' &&
    'getWheelTarget' in value &&
    typeof value.getWheelTarget === 'function'
  );
}

function routeForXtermTarget(target: XtermWheelTarget): WheelRoute {
  return { kind: 'xterm', target };
}

function resolveDeclaredWheelRoute(
  decl: CardWheelTargetDecl | null,
): WheelRoute | null {
  if (!decl) return null;
  if (decl.kind === 'sink') return { kind: 'sink' };
  if (decl.kind === 'native-scroll') {
    const target = decl.ref.current;
    return target instanceof HTMLElement
      ? { kind: 'native-scroll', target }
      : { kind: 'page' };
  }
  const handle = decl.ref.current;
  if (!isXtermViewHandle(handle)) return { kind: 'sink' };
  const target = handle.getWheelTarget();
  return target ? routeForXtermTarget(target) : { kind: 'sink' };
}

export function resolveWheelRoute(args: {
  scrollRoot: HTMLElement;
  activeCard: HTMLElement | null;
  eventTarget: EventTarget | null;
  resolveCardById?: ResolveCardById;
}): WheelRoute {
  const { scrollRoot, activeCard, eventTarget, resolveCardById } = args;
  const target = asElement(eventTarget);

  if (target?.closest(MODAL_SELECTOR)) return { kind: 'page' };
  if (!activeCard || !scrollRoot.contains(activeCard)) return { kind: 'page' };

  const targetInsideActiveCard =
    target !== null && (target === activeCard || activeCard.contains(target));
  if (targetInsideActiveCard) {
    const scrollable = closestWithin(target, activeCard, isScrollableY);
    if (scrollable) return { kind: 'native-scroll', target: scrollable };
  }

  const cardId = activeCard.dataset.cardId;
  if (cardId && resolveCardById) {
    const resolved = resolveCardById(cardId);
    const entry = resolved ? getEntry(resolved.card.type) : undefined;
    if (entry?.wheelTarget && resolved) {
      const route = resolveDeclaredWheelRoute(
        entry.wheelTarget(resolved.card, resolved.instance),
      );
      if (route) return route;
    }
  }

  const xtermTarget = getXtermForShell(activeCard);
  if (xtermTarget) return routeForXtermTarget(xtermTarget);
  if (activeCard.querySelector<HTMLElement>(XTERM_ROOT_SELECTOR)) {
    return { kind: 'sink' };
  }

  return { kind: 'sink' };
}
