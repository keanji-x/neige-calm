// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';

type GridCapture = {
  layout: Array<{ i: string; x: number; y: number; w: number; h: number; minW?: number; minH?: number }>;
  dragHandle: string | undefined;
  resizeHandles: readonly string[] | undefined;
  onLayoutChange: ((next: GridCapture['layout']) => void) | null;
};
const grid: GridCapture = {
  layout: [],
  dragHandle: undefined,
  resizeHandles: undefined,
  onLayoutChange: null,
};

vi.mock('react-grid-layout', () => ({
  useContainerWidth: () => ({
    width: 1200,
    containerRef: { current: null },
    mounted: true,
  }),
  GridLayout: (props: {
    layout: GridCapture['layout'];
    dragConfig?: { handle?: string };
    resizeConfig?: { handles?: readonly string[] };
    onLayoutChange: GridCapture['onLayoutChange'];
    children?: ReactNode;
  }) => {
    grid.layout = props.layout;
    grid.dragHandle = props.dragConfig?.handle;
    grid.resizeHandles = props.resizeConfig?.handles;
    grid.onLayoutChange = props.onLayoutChange;
    return <div data-testid="grid-stub">{props.children}</div>;
  },
}));

import type { CardEntry } from '../registry.js';
import { createCardHost, createCardRegistry } from '../public.js';
import { CardHead } from './card-head.tsx';
import { BoardHost } from './board-host.tsx';

declare module '../registry.js' {
  interface CardDataMap {
    boardHostTerm: { type: 'board-host-term'; id: string; title: string | null; terminalId: string | null };
  }
}

const size = Object.freeze({ w: 6, h: 10, minW: 4, minH: 6 });
const entry: CardEntry = {
  type: 'board-host-term',
  component: () => (
    <div className="term live">
      <CardHead className="card-drag-handle" title="Build log" />
      <div className="term-body">term body</div>
    </div>
  ),
  defaultSize: size,
  title: () => 'Terminal',
  accessibleName: () => 'Terminal',
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
};

function renderBoard() {
  const registry = createCardRegistry();
  registry.register(entry);
  const host = createCardHost(registry);
  const card = { type: 'board-host-term' as const, id: 'card-a', title: null, terminalId: 't1' };
  return render(
    <BoardHost
      host={host}
      items={[Object.freeze({ card, title: 'Build log', originalIndex: 0 })]}
      activeCardId="card-a"
      visible
    />,
  );
}

beforeEach(() => {
  grid.layout = [];
  grid.dragHandle = undefined;
  grid.resizeHandles = undefined;
  grid.onLayoutChange = null;
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 1;
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('BoardHost react-grid-layout wiring', () => {
  it('packs a terminal at the TrackGrid default 6×10 and enables SE resize plus header drag', () => {
    renderBoard();
    expect(grid.layout).toEqual([
      { i: 'card-a', x: 0, y: 0, w: 6, h: 10, minW: 4, minH: 6 },
    ]);
    expect(grid.dragHandle).toBe('.card-drag-handle');
    expect(grid.resizeHandles).toEqual(['se']);
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-a"]')).toBeTruthy();
    expect(screen.getByText('Build log')).toBeTruthy();
  });

  it('keeps dragged positions across synchronous animation-frame callbacks', () => {
    renderBoard();
    act(() => {
      grid.onLayoutChange?.([{ i: 'card-a', x: 4, y: 2, w: 6, h: 8 }]);
    });
    expect(grid.layout).toEqual([
      { i: 'card-a', x: 4, y: 2, w: 6, h: 8, minW: 4, minH: 6 },
    ]);

    act(() => {
      grid.onLayoutChange?.([{ i: 'card-a', x: 2, y: 3, w: 6, h: 9 }]);
    });
    expect(grid.layout).toEqual([
      { i: 'card-a', x: 2, y: 3, w: 6, h: 9, minW: 4, minH: 6 },
    ]);
  });

  it('cancels the held timeout fallback without updating after unmount', () => {
    vi.stubGlobal('requestAnimationFrame', undefined);
    vi.stubGlobal('cancelAnimationFrame', undefined);

    const timeoutHandle = 47;
    const heldTimeouts = new Map<number, () => void>();
    const scheduledCallback = vi.fn();
    const setTimeoutStub = vi.fn((callback: TimerHandler) => {
      if (typeof callback !== 'function') throw new Error('Expected a function timer callback');
      const invoke = callback as () => void;
      heldTimeouts.set(timeoutHandle, vi.fn(() => {
        scheduledCallback();
        invoke();
      }));
      return timeoutHandle;
    });
    const clearTimeoutStub = vi.fn((handle: number | undefined) => {
      if (handle !== undefined) heldTimeouts.delete(handle);
    });
    vi.stubGlobal('setTimeout', setTimeoutStub);
    vi.stubGlobal('clearTimeout', clearTimeoutStub);

    const board = renderBoard();
    const layoutBeforeUnmount = grid.layout;
    act(() => {
      grid.onLayoutChange?.([{ i: 'card-a', x: 3, y: 2, w: 6, h: 8 }]);
    });

    expect(setTimeoutStub).toHaveBeenCalledOnce();
    expect.soft(() => board.unmount()).not.toThrow();
    expect.soft(clearTimeoutStub).toHaveBeenCalledWith(timeoutHandle);

    act(() => {
      const due = Array.from(heldTimeouts.values());
      heldTimeouts.clear();
      for (const callback of due) callback();
    });
    expect.soft(scheduledCallback).not.toHaveBeenCalled();
    expect(grid.layout).toBe(layoutBeforeUnmount);
  });
});

describe('BoardHost lifecycle', () => {
  it('replays the current visible and focused state when replacing the host', () => {
    const registry = createCardRegistry();
    registry.register(entry);
    const hostA = createCardHost(registry);
    const hostB = createCardHost(registry);
    const card = { type: 'board-host-term' as const, id: 'card-a', title: null, terminalId: 't1' };
    const items = [Object.freeze({ card, title: 'Build log', originalIndex: 0 })];
    const board = render(
      <BoardHost host={hostA} items={items} activeCardId="card-a" visible />,
    );

    expect(hostA.resolve('card-a')?.lifecycle.getSnapshot()).toMatchObject({
      visible: true,
      focused: true,
    });

    board.rerender(
      <BoardHost host={hostB} items={items} activeCardId="card-a" visible />,
    );

    expect(hostA.resolve('card-a')).toBeNull();
    expect(hostB.resolve('card-a')?.lifecycle.getSnapshot()).toMatchObject({
      visible: true,
      focused: true,
    });

    board.unmount();
    expect(hostB.resolve('card-a')).toBeNull();
  });
});

/*
 * ── The head's delete ───────────────────────────────────────────────────────
 *
 * `onRemove` reaches the component **already resolved**: the entry is never
 * asked to decide whether the card may be deleted, so these cases assert on the
 * prop the component receives rather than on any bit it reads for itself. That
 * is what keeps one rule ("the kernel owns `deletable`") in one place instead of
 * once per card kind.
 */
describe('BoardHost card removal', () => {
  const removable: CardEntry = {
    ...entry,
    component: ({ onRemove }) => (
      <div className="term live">
        <CardHead
          className="card-drag-handle"
          title="Build log"
          onClose={onRemove}
          closeAriaLabel="Delete card Build log"
        />
        <div className="term-body">term body</div>
      </div>
    ),
  };

  function renderRemovable(options: {
    onRemoveCard?: (cardId: string) => void;
    deletable?: boolean;
  }) {
    const registry = createCardRegistry();
    registry.register(removable);
    const host = createCardHost(registry);
    const card = { type: 'board-host-term' as const, id: 'card-a', title: null, terminalId: 't1' };
    return render(
      <BoardHost
        host={host}
        items={[Object.freeze({
          card, title: 'Build log', originalIndex: 0, deletable: options.deletable,
        })]}
        activeCardId="card-a"
        visible
        onRemoveCard={options.onRemoveCard}
      />,
    );
  }

  it('hands the component a remove bound to its own card id', async () => {
    const onRemoveCard = vi.fn();
    renderRemovable({ onRemoveCard, deletable: true });
    await userEvent.click(screen.getByRole('button', { name: 'Delete card Build log' }));
    expect(onRemoveCard).toHaveBeenCalledWith('card-a');
  });

  it('draws no delete when the board was given none', () => {
    renderRemovable({ deletable: true });
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
  });

  it('withholds the delete on a kernel-owned card', () => {
    renderRemovable({ onRemoveCard: vi.fn(), deletable: false });
    expect(screen.queryByRole('button', { name: 'Delete card Build log' })).toBeNull();
  });

  /* A wire row from a server newer than this bundle carries no `deletable`
     field at all. `cardWireSchema` reads that omission as "user-deletable", and
     the board must not read it as the opposite — a card nobody can delete and
     no entry can draw is unreachable in both directions. */
  it('treats an absent deletable bit as deletable', async () => {
    const onRemoveCard = vi.fn();
    renderRemovable({ onRemoveCard });
    await userEvent.click(screen.getByRole('button', { name: 'Delete card Build log' }));
    expect(onRemoveCard).toHaveBeenCalledWith('card-a');
  });

  /* The unknown-card fallback draws its own head, so it needs its own case:
     no entry claims this kind, which makes the × the only control on the board
     that can act on it at all. */
  it('puts a delete on the unknown-card fallback head', async () => {
    const onRemoveCard = vi.fn();
    const registry = createCardRegistry();
    const host = createCardHost(registry);
    const card = { type: 'board-host-term' as const, id: 'card-z', title: null, terminalId: null };
    render(
      <BoardHost
        host={host}
        items={[Object.freeze({ card, title: 'Mystery', originalIndex: 0 })]}
        activeCardId={null}
        visible
        onRemoveCard={onRemoveCard}
      />,
    );
    await userEvent.click(screen.getByRole('button', { name: 'Delete card Mystery' }));
    expect(onRemoveCard).toHaveBeenCalledWith('card-z');
  });
});
