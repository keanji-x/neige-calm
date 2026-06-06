import { readFileSync } from 'node:fs';
import { cleanup, render } from '@testing-library/react';
import { createElement, useRef } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { CardWheelTargetDecl } from '../cards/lifecycle';
import type {
  CardEntryResolverValue,
  CardLifecycleWriter,
} from '../cards/resolver';
import {
  __resetRegistryForTest,
  registerCard,
  type CardEntry,
  type CardInstanceCtx,
} from '../cards/registry';
import type { WaveCardData } from '../types';
import {
  getActiveCardShell,
  pixelDelta,
  resolveWheelRoute,
  type WheelRoute,
} from './wheelRouter';
import type { XtermWheelTarget } from './xtermAdapter';
import { useWheelRouter } from './useWheelRouter';
import { registerXtermShell, unregisterXtermShell } from './wheelTargets';

type WheelInstance = Pick<CardInstanceCtx, 'cardId' | 'useCardSlot'>;

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

function fakeInstance(): WheelInstance {
  const slots = new Map<string, unknown>();
  return {
    cardId: 'card_1',
    useCardSlot<S>(key: string, initial: S | (() => S)) {
      if (!slots.has(key)) {
        slots.set(
          key,
          typeof initial === 'function' ? (initial as () => S)() : initial,
        );
      }
      const setValue = (next: S | ((prev: S) => S)) => {
        const current = slots.get(key) as S;
        slots.set(
          key,
          typeof next === 'function'
            ? (next as (prev: S) => S)(current)
            : next,
        );
      };
      return [slots.get(key) as S, setValue];
    },
  };
}

function fakeCard(): WaveCardData {
  return {
    type: 'terminal',
    id: 'card_1',
    title: 'terminal',
    lines: [],
  } as unknown as WaveCardData;
}

function fakeLifecycleWriter(): CardLifecycleWriter {
  return {
    getSnapshot: () => ({
      visible: true,
      focused: false,
      geometry: { width: 0, height: 0, ready: false },
      refreshEpoch: 0,
    }),
    subscribe: () => () => {},
    setVisible: () => {},
    setFocused: () => {},
    setGeometry: () => {},
    bumpRefresh: () => {},
  };
}

function fakeResolvedCard(): CardEntryResolverValue {
  return {
    card: fakeCard(),
    instance: fakeInstance(),
    writer: fakeLifecycleWriter(),
  };
}

function registerWheelEntry(
  wheelTarget: (
    card: WaveCardData,
    instance: WheelInstance,
  ) => CardWheelTargetDecl | null,
) {
  registerCard({
    type: 'terminal',
    Component: () => null,
    defaultSize: { w: 1, h: 1, minW: 1, minH: 1 },
    title: () => 'terminal',
    accessibleName: () => 'terminal',
    create: { mode: 'kernel-minted-only' },
    wheelTarget,
  } as unknown as CardEntry);
}

afterEach(() => {
  cleanup();
  __resetRegistryForTest();
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
      }),
    ).toEqual({ kind: 'page' });
  });

  it('routes to a registered xterm wheel target', () => {
    const { scrollRoot, activeCard } = fixture();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide: () => ({ kind: 'consume' }),
      apply: () => undefined,
    };
    registerXtermShell(activeCard, xtermTarget);

    const route: WheelRoute = resolveWheelRoute({
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
    });

    expect(route).toEqual({ kind: 'xterm', target: xtermTarget });
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
      }),
    ).toEqual({ kind: 'sink' });
  });

  it('keeps in-card native scrollable routing before entry declarations', () => {
    const { scrollRoot, activeCard } = fixture();
    activeCard.dataset.cardId = 'card_1';
    const scroller = document.createElement('div');
    scroller.style.overflowY = 'auto';
    setScrollSize(scroller, 400, 100);
    activeCard.append(scroller);
    registerWheelEntry(() => ({ kind: 'sink' }));

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: scroller,
        resolveCardById: fakeResolvedCard,
      }),
    ).toEqual({ kind: 'native-scroll', target: scroller });
  });

  it('routes entry-declared xterm wheelTarget to xterm', () => {
    const { scrollRoot, activeCard } = fixture();
    activeCard.dataset.cardId = 'card_1';
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide: () => ({ kind: 'consume' }),
      apply: () => undefined,
    };
    const handle = { getWheelTarget: () => xtermTarget };
    registerWheelEntry(() => ({ kind: 'xterm', ref: { current: handle } }));

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: scrollRoot,
        resolveCardById: fakeResolvedCard,
      }),
    ).toEqual({
      kind: 'xterm',
      target: xtermTarget,
    });
  });

  it('sinks entry-declared xterm wheelTarget when ref is not wired yet', () => {
    const { scrollRoot, activeCard } = fixture();
    activeCard.dataset.cardId = 'card_1';
    registerWheelEntry(() => ({ kind: 'xterm', ref: { current: null } }));

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: scrollRoot,
        resolveCardById: fakeResolvedCard,
      }),
    ).toEqual({ kind: 'sink' });
  });

  it('routes entry-declared native-scroll wheelTarget or page when ref is null', () => {
    const { scrollRoot, activeCard } = fixture();
    activeCard.dataset.cardId = 'card_1';
    const scroller = document.createElement('div');
    const ref: { current: HTMLElement | null } = { current: scroller };
    registerWheelEntry(() => ({ kind: 'native-scroll', ref }));
    const args = {
      scrollRoot,
      activeCard,
      eventTarget: scrollRoot,
      resolveCardById: fakeResolvedCard,
    };

    expect(resolveWheelRoute(args)).toEqual({
      kind: 'native-scroll',
      target: scroller,
    });

    ref.current = null;
    expect(resolveWheelRoute(args)).toEqual({ kind: 'page' });
  });

  it('routes entry-declared sink wheelTarget to sink', () => {
    const { scrollRoot, activeCard } = fixture();
    activeCard.dataset.cardId = 'card_1';
    registerWheelEntry(() => ({ kind: 'sink' }));

    expect(
      resolveWheelRoute({
        scrollRoot,
        activeCard,
        eventTarget: scrollRoot,
        resolveCardById: fakeResolvedCard,
      }),
    ).toEqual({ kind: 'sink' });
  });

  it('falls back to WeakMap xterm routing when shell has no data-card-id', () => {
    const { scrollRoot, activeCard } = fixture();
    const resolveCardById = vi.fn(fakeResolvedCard);
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide: () => ({ kind: 'consume' }),
      apply: () => undefined,
    };
    registerXtermShell(activeCard, xtermTarget);

    try {
      expect(
        resolveWheelRoute({
          scrollRoot,
          activeCard,
          eventTarget: scrollRoot,
          resolveCardById,
        }),
      ).toEqual({ kind: 'xterm', target: xtermTarget });
      expect(resolveCardById).not.toHaveBeenCalled();
    } finally {
      unregisterXtermShell(activeCard);
    }
  });

  it('does not carry legacy file-viewer selector literals in the router', () => {
    const source = readFileSync('src/input/wheelRouter.ts', 'utf8');

    expect(source).not.toMatch(
      /\.cm-scroller|\.file-viewer-tree-list|\.file-viewer-changes|\.file-viewer-merge|data-wheel-file-viewer-tab/,
    );
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

  it("falls through when xterm decides pass with reason 'passthrough'", () => {
    const { scrollRoot, activeCard } = fixture();
    const decide = vi.fn(() => ({
      kind: 'pass' as const,
      reason: 'passthrough' as const,
    }));
    const apply = vi.fn();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide,
      apply,
    };
    registerXtermShell(activeCard, xtermTarget);
    const restore = mockElementFromPoint(activeCard);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot);

      expect(event.defaultPrevented).toBe(false);
      expect(decide).toHaveBeenCalledWith(120, WheelEvent.DOM_DELTA_PIXEL);
      expect(apply).not.toHaveBeenCalled();
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

  it('prevents default and applies when xterm consumes', () => {
    const { scrollRoot, activeCard } = fixture();
    const decide = vi.fn(() => ({ kind: 'consume' as const }));
    const apply = vi.fn();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide,
      apply,
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
      expect(decide).toHaveBeenCalledWith(48, WheelEvent.DOM_DELTA_LINE);
      expect(apply).toHaveBeenCalledWith(48, WheelEvent.DOM_DELTA_LINE);
    } finally {
      restore();
      unregisterXtermShell(activeCard);
    }
  });

  it("falls through when xterm decides pass with reason 'edge'", () => {
    const { scrollRoot, activeCard } = fixture();
    const decide = vi.fn(() => ({
      kind: 'pass' as const,
      reason: 'edge' as const,
    }));
    const apply = vi.fn();
    const xtermTarget: XtermWheelTarget = {
      root: document.createElement('div'),
      decide,
      apply,
    };
    registerXtermShell(activeCard, xtermTarget);
    const restore = mockElementFromPoint(activeCard);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(scrollRoot, {
        deltaY: 48,
        deltaMode: WheelEvent.DOM_DELTA_LINE,
      });

      expect(event.defaultPrevented).toBe(false);
      expect(decide).toHaveBeenCalledWith(48, WheelEvent.DOM_DELTA_LINE);
      expect(apply).not.toHaveBeenCalled();
    } finally {
      restore();
      unregisterXtermShell(activeCard);
    }
  });

  it('prevents default for sink routes', () => {
    const { scrollRoot, activeCard } = fixture();
    const body = document.createElement('div');
    activeCard.append(body);
    const restore = mockElementFromPoint(body);
    mountWheelRouter(scrollRoot);

    try {
      const event = dispatchWheel(body);

      expect(event.defaultPrevented).toBe(true);
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
        }),
      ).toEqual({ kind: 'page' });
    } finally {
      restore();
    }
  });
});
