import { cleanup, render } from '@testing-library/react';
import { createElement, useRef } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
  type WheelRoute,
} from './wheelRouter';
import type { XtermWheelTarget } from './xtermAdapter';
import { useWheelRouter } from './useWheelRouter';
import { registerXtermShell, unregisterXtermShell } from './wheelTargets';

function setScrollSize(
  el: HTMLElement,
  scrollHeight: number,
  clientHeight: number,
) {
  Object.defineProperty(el, 'scrollHeight', {
    configurable: true,
    value: scrollHeight,
  });
  Object.defineProperty(el, 'clientHeight', {
    configurable: true,
    value: clientHeight,
  });
}

function fixture() {
  const scrollRoot = document.createElement('div');
  const activeCard = document.createElement('section');
  activeCard.dataset.wheelCard = '';
  scrollRoot.append(activeCard);
  document.body.append(scrollRoot);
  return { scrollRoot, activeCard };
}

function mockElementFromPoint(el: Element | null) {
  const orig = document.elementFromPoint;
  document.elementFromPoint = () => el;
  return () => {
    document.elementFromPoint = orig;
  };
}

function WheelRouterHarness({ scrollRoot }: { scrollRoot: HTMLElement }) {
  const scrollRef = useRef<HTMLElement | null>(scrollRoot);
  useWheelRouter(scrollRef);
  return null;
}

function mountWheelRouter(scrollRoot: HTMLElement) {
  render(createElement(WheelRouterHarness, { scrollRoot }));
}

function dispatchWheel(target: Element, init: WheelEventInit = {}): WheelEvent {
  const event = new WheelEvent('wheel', {
    bubbles: true,
    cancelable: true,
    clientX: 1,
    clientY: 1,
    deltaY: 120,
    deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    ...init,
  });
  target.dispatchEvent(event);
  return event;
}

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
});

describe('pixelDelta', () => {
  it('keeps pixel-mode wheel deltas unchanged on both axes', () => {
    const event = new WheelEvent('wheel', {
      deltaX: 4,
      deltaY: -8,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(pixelDelta(event)).toEqual({ x: 4, y: -8 });
  });

  it('normalizes line-mode wheel deltas to pixels on both axes', () => {
    const event = new WheelEvent('wheel', {
      deltaX: 2,
      deltaY: -3,
      deltaMode: WheelEvent.DOM_DELTA_LINE,
    });

    expect(pixelDelta(event)).toEqual({ x: 32, y: -48 });
  });

  it('normalizes page-mode wheel deltas from the current target height', () => {
    const root = document.createElement('div');
    setScrollSize(root, 1000, 250);
    let delta: { x: number; y: number } | null = null;
    root.addEventListener('wheel', (event) => {
      delta = pixelDelta(event);
    });

    root.dispatchEvent(
      new WheelEvent('wheel', {
        deltaX: 1,
        deltaY: -2,
        deltaMode: WheelEvent.DOM_DELTA_PAGE,
      }),
    );

    expect(delta).toEqual({ x: 250, y: -500 });
  });
});

describe('resolveWheelRoute', () => {
  it('returns page when no card is active', () => {
    const { scrollRoot } = fixture();

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard: null,
        eventTarget: scrollRoot,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'page' });
  });

  it('routes to a native scrollable textarea inside the active card', () => {
    const { scrollRoot, activeCard } = fixture();
    const textarea = document.createElement('textarea');
    textarea.style.overflowY = 'auto';
    setScrollSize(textarea, 400, 100);
    activeCard.append(textarea);

    const route = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: textarea,
      deltaY: 120,
    });

    expect(route).toEqual({ kind: 'native-scroll', target: textarea });
  });

  it('sinks a card with no native scrollable target', () => {
    const { scrollRoot, activeCard } = fixture();
    const body = document.createElement('div');
    activeCard.append(body);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: body,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'sink' });
  });

  it('lets modal targets keep their dialog scroll behavior', () => {
    const { scrollRoot, activeCard } = fixture();
    const modal = document.createElement('div');
    modal.className = 'modal-overlay';
    const panel = document.createElement('div');
    modal.append(panel);
    document.body.append(modal);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: panel,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'page' });
  });

  it('routes to a registered xterm wheel target', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: () => true,
      scrollback: () => true,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: 120,
    });

    expect(route).toEqual({ kind: 'xterm-scrollback', target: xtermTarget });
    unregisterXtermShell(activeCard);
  });

  it('routes xterm passthrough when xterm should handle wheel natively', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'passthrough',
      canScrollback: () => false,
      scrollback: () => false,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: 120,
    });

    expect(route).toEqual({ kind: 'xterm-passthrough', target: xtermTarget });
    unregisterXtermShell(activeCard);
  });

  it('sinks xterm with no scrollback', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: () => false,
      scrollback: () => false,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: 120,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(route).toEqual({ kind: 'sink' });
    unregisterXtermShell(activeCard);
  });

  it('sinks xterm at the top of scrollback wheel up', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: (deltaY) => deltaY > 0,
      scrollback: () => false,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: -120,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(route).toEqual({ kind: 'sink' });
    unregisterXtermShell(activeCard);
  });

  it('routes xterm at the top of scrollback wheel down to xterm scrollback', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: (deltaY) => deltaY > 0,
      scrollback: () => true,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: 120,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(route).toEqual({ kind: 'xterm-scrollback', target: xtermTarget });
    unregisterXtermShell(activeCard);
  });

  it('routes xterm at the bottom of scrollback wheel up to xterm scrollback', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: (deltaY) => deltaY < 0,
      scrollback: () => true,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: -120,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(route).toEqual({ kind: 'xterm-scrollback', target: xtermTarget });
    unregisterXtermShell(activeCard);
  });

  it('sinks xterm at the bottom of scrollback wheel down', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: (deltaY) => deltaY < 0,
      scrollback: () => false,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      deltaY: 120,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
    });

    expect(route).toEqual({ kind: 'sink' });
    unregisterXtermShell(activeCard);
  });

  it('sinks an xterm card when the xterm handle has not registered yet', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermRoot = document.createElement('div');
    xtermRoot.className = 'xterm-view';
    activeCard.append(xtermRoot);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: scrollRoot,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'sink' });
  });

  it('routes file-viewer wheel over CodeMirror to the cm scroller', () => {
    const { scrollRoot, activeCard } = fixture();
    const viewer = document.createElement('div');
    viewer.className = 'file-viewer-card';
    viewer.dataset.wheelFileViewerTab = 'code';
    const cmScroller = document.createElement('div');
    cmScroller.className = 'cm-scroller';
    viewer.append(cmScroller);
    activeCard.append(viewer);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: cmScroller,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'native-scroll', target: cmScroller });
  });

  it('routes file-viewer wheel over the file tree to the tree list', () => {
    const { scrollRoot, activeCard } = fixture();
    const viewer = document.createElement('div');
    viewer.className = 'file-viewer-card';
    viewer.dataset.wheelFileViewerTab = 'code';
    const treeList = document.createElement('div');
    treeList.className = 'file-viewer-tree-list';
    const treeEntry = document.createElement('button');
    treeList.append(treeEntry);
    viewer.append(treeList);
    activeCard.append(viewer);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: treeEntry,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'native-scroll', target: treeList });
  });

  it('falls back to the CodeMirror scroller for file-viewer code tab chrome', () => {
    const { scrollRoot, activeCard } = fixture();
    const viewer = document.createElement('div');
    viewer.className = 'file-viewer-card';
    viewer.dataset.wheelFileViewerTab = 'code';
    const toolbar = document.createElement('div');
    toolbar.className = 'file-viewer-toolbar';
    const cmScroller = document.createElement('div');
    cmScroller.className = 'cm-scroller';
    viewer.append(toolbar, cmScroller);
    activeCard.append(viewer);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: toolbar,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'native-scroll', target: cmScroller });
  });

  it('falls back to the merge pane for file-viewer diff tab chrome', () => {
    const { scrollRoot, activeCard } = fixture();
    const viewer = document.createElement('div');
    viewer.className = 'file-viewer-card';
    viewer.dataset.wheelFileViewerTab = 'diff';
    const toolbar = document.createElement('div');
    toolbar.className = 'file-viewer-toolbar';
    const changes = document.createElement('div');
    changes.className = 'file-viewer-changes';
    const merge = document.createElement('div');
    merge.className = 'file-viewer-merge';
    viewer.append(toolbar, changes, merge);
    activeCard.append(viewer);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: toolbar,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'native-scroll', target: merge });
  });

  it('falls back to changed files when diff tab has no merge pane', () => {
    const { scrollRoot, activeCard } = fixture();
    const viewer = document.createElement('div');
    viewer.className = 'file-viewer-card';
    viewer.dataset.wheelFileViewerTab = 'diff';
    const toolbar = document.createElement('div');
    toolbar.className = 'file-viewer-toolbar';
    const changes = document.createElement('div');
    changes.className = 'file-viewer-changes';
    viewer.append(toolbar, changes);
    activeCard.append(viewer);

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: toolbar,
        deltaY: 120,
      }),
    ).toEqual({ kind: 'native-scroll', target: changes });
  });
});

describe('useWheelRouter', () => {
  it('lets page routes keep native browser scroll behavior', () => {
    const { scrollRoot } = fixture();
    const outside = document.createElement('div');
    document.body.append(outside);
    const restore = mockElementFromPoint(outside);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot);

      expect(event.defaultPrevented).toBe(false);
    } finally {
      restore();
    }
  });

  it('lets xterm passthrough routes keep native xterm wheel behavior', () => {
    const { scrollRoot, activeCard } = fixture();
    const scrollback = vi.fn(() => false);
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'passthrough',
      canScrollback: () => false,
      scrollback,
    };
    registerXtermShell(activeCard, xtermTarget);
    const restore = mockElementFromPoint(activeCard);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot);

      expect(event.defaultPrevented).toBe(false);
      expect(scrollback).not.toHaveBeenCalled();
    } finally {
      restore();
      unregisterXtermShell(activeCard);
    }
  });

  it('prevents default and scrolls native scroll targets', () => {
    const { scrollRoot, activeCard } = fixture();
    const scroller = document.createElement('div');
    scroller.style.overflowY = 'auto';
    setScrollSize(scroller, 400, 100);
    activeCard.append(scroller);
    const restore = mockElementFromPoint(scroller);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scroller, { deltaY: 32 });

      expect(event.defaultPrevented).toBe(true);
      expect(scroller.scrollTop).toBe(32);
    } finally {
      restore();
    }
  });

  it('prevents default when xterm scrollback consumes the wheel', () => {
    const { scrollRoot, activeCard } = fixture();
    const scrollback = vi.fn(() => true);
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: () => true,
      scrollback,
    };
    registerXtermShell(activeCard, xtermTarget);
    const restore = mockElementFromPoint(activeCard);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot, {
        deltaY: 48,
        deltaMode: WheelEvent.DOM_DELTA_LINE,
      });

      expect(event.defaultPrevented).toBe(true);
      expect(scrollback).toHaveBeenCalledWith(48, WheelEvent.DOM_DELTA_LINE);
    } finally {
      restore();
      unregisterXtermShell(activeCard);
    }
  });

  it('sinks when xterm scrollback cannot consume the wheel', () => {
    const { scrollRoot, activeCard } = fixture();
    const scrollback = vi.fn(() => false);
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      mode: () => 'scrollback',
      canScrollback: () => false,
      scrollback,
    };
    registerXtermShell(activeCard, xtermTarget);
    const restore = mockElementFromPoint(activeCard);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot, {
        deltaY: 48,
        deltaMode: WheelEvent.DOM_DELTA_LINE,
      });

      expect(event.defaultPrevented).toBe(true);
      expect(scrollback).not.toHaveBeenCalled();
      expect(scrollRoot.scrollTop).toBe(0);
    } finally {
      restore();
      unregisterXtermShell(activeCard);
    }
  });

  it('sinks cards with no in-card wheel target', () => {
    const { scrollRoot, activeCard } = fixture();
    const body = document.createElement('div');
    activeCard.append(body);
    const restore = mockElementFromPoint(body);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(body);

      expect(event.defaultPrevented).toBe(true);
      expect(scrollRoot.scrollTop).toBe(0);
    } finally {
      restore();
    }
  });

  it('sinks xterm cards before their handle registers', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermRoot = document.createElement('div');
    xtermRoot.className = 'xterm-view';
    activeCard.append(xtermRoot);
    const restore = mockElementFromPoint(xtermRoot);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(xtermRoot);

      expect(event.defaultPrevented).toBe(true);
      expect(scrollRoot.scrollTop).toBe(0);
    } finally {
      restore();
    }
  });
});

describe('getActiveCardShell', () => {
  it('returns the cursor-pointed card shell inside the scroll root', () => {
    const { scrollRoot, activeCard } = fixture();
    const body = document.createElement('div');
    activeCard.append(body);
    const restore = mockElementFromPoint(body);

    try {
      expect(getActiveCardShell(scrollRoot, document, 0, 0)).toBe(activeCard);
    } finally {
      restore();
    }
  });

  it("returns null when the cursor isn't over a card so wheel routes to page", () => {
    const { scrollRoot } = fixture();
    const outside = document.createElement('div');
    scrollRoot.append(outside);
    const restore = mockElementFromPoint(outside);

    try {
      const activeCard = getActiveCardShell(scrollRoot, document, 0, 0);

      expect(activeCard).toBeNull();
      expect(
        resolveWheelRoute({
          scrollRoot,
          activeCard,
          eventTarget: outside,
          deltaY: 120,
        }),
      ).toEqual({ kind: 'page' });
    } finally {
      restore();
    }
  });
});
