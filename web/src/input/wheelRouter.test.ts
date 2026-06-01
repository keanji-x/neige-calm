import { afterEach, describe, expect, it } from 'vitest';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
  type WheelRoute,
} from './wheelRouter';
import type { XtermWheelTarget } from './xtermAdapter';
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

afterEach(() => {
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

  it('sinks when the active card has no native scrollable target or xterm hint', () => {
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
      scrollback: () => undefined,
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
      scrollback: () => undefined,
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
