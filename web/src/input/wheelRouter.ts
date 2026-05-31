import type { XtermWheelTarget } from './xtermAdapter';
import { getXtermForShell } from './wheelTargets';

export type WheelRoute =
  | { kind: 'page' }
  | { kind: 'native-scroll'; target: HTMLElement }
  | { kind: 'xterm'; target: XtermWheelTarget }
  | { kind: 'sink' };

const MODAL_SELECTOR = '.modal-overlay, .modal-panel';
const WHEEL_CARD_SELECTOR = '[data-wheel-card]';
const XTERM_ROOT_SELECTOR = '.xterm-view';
const FILE_VIEWER_CARD_SELECTOR = '.file-viewer-card';
const FILE_VIEWER_PANE_SELECTOR =
  '.cm-scroller, .file-viewer-tree-list, .file-viewer-changes, .file-viewer-merge';

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

function fileViewerPaneFor(
  activeCard: HTMLElement,
  target: Element | null,
): HTMLElement | null {
  const fileViewer = activeCard.querySelector<HTMLElement>(
    FILE_VIEWER_CARD_SELECTOR,
  );
  if (!fileViewer) return null;

  const closestPane = target?.closest<HTMLElement>(FILE_VIEWER_PANE_SELECTOR);
  if (closestPane && activeCard.contains(closestPane)) return closestPane;

  const tab = fileViewer.dataset.wheelFileViewerTab;
  if (tab === 'code') {
    return activeCard.querySelector<HTMLElement>('.cm-scroller');
  }
  if (tab === 'diff') {
    return (
      activeCard.querySelector<HTMLElement>('.file-viewer-merge') ??
      activeCard.querySelector<HTMLElement>('.file-viewer-changes')
    );
  }
  return null;
}

export function getActiveCardShell(
  scrollRoot: HTMLElement,
  ownerDocument: Document,
): HTMLElement | null {
  const focusedShell = ownerDocument.querySelector<HTMLElement>(
    `${WHEEL_CARD_SELECTOR}:focus-within`,
  );
  const activeCard =
    focusedShell ??
    (ownerDocument.activeElement instanceof HTMLElement
      ? ownerDocument.activeElement.closest<HTMLElement>(WHEEL_CARD_SELECTOR)
      : null);
  if (!activeCard) return null;
  return scrollRoot.contains(activeCard) ? activeCard : null;
}

export function resolveWheelRoute(args: {
  scrollRoot: HTMLElement;
  activeCard: HTMLElement | null;
  eventTarget: EventTarget | null;
  deltaY: number;
}): WheelRoute {
  const { scrollRoot, activeCard, eventTarget } = args;
  const target = asElement(eventTarget);

  if (target?.closest(MODAL_SELECTOR)) return { kind: 'page' };
  if (!activeCard || !scrollRoot.contains(activeCard)) return { kind: 'page' };

  const targetInsideActiveCard =
    target !== null && (target === activeCard || activeCard.contains(target));
  if (targetInsideActiveCard) {
    const scrollable = closestWithin(target, activeCard, isScrollableY);
    if (scrollable) return { kind: 'native-scroll', target: scrollable };
  }

  const xtermTarget = getXtermForShell(activeCard);
  if (xtermTarget?.canHandleWheel()) {
    return { kind: 'xterm', target: xtermTarget };
  }
  if (activeCard.querySelector<HTMLElement>(XTERM_ROOT_SELECTOR)) {
    return { kind: 'sink' };
  }

  const fileViewerPane = fileViewerPaneFor(activeCard, target);
  if (fileViewerPane) {
    return { kind: 'native-scroll', target: fileViewerPane };
  }

  return { kind: 'sink' };
}
