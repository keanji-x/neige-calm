// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
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
  it('packs a terminal at the WaveGrid default 6×10 and enables SE resize plus header drag', () => {
    renderBoard();
    expect(grid.layout).toEqual([
      { i: 'card-a', x: 0, y: 0, w: 6, h: 10, minW: 4, minH: 6 },
    ]);
    expect(grid.dragHandle).toBe('.card-drag-handle');
    expect(grid.resizeHandles).toEqual(['se']);
    expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="card-a"]')).toBeTruthy();
    expect(screen.getByText('Build log')).toBeTruthy();
  });

  it('keeps a dragged position on the next layout pass', () => {
    renderBoard();
    act(() => {
      grid.onLayoutChange?.([{ i: 'card-a', x: 4, y: 2, w: 6, h: 8 }]);
    });
    expect(grid.layout).toEqual([
      { i: 'card-a', x: 4, y: 2, w: 6, h: 8, minW: 4, minH: 6 },
    ]);
  });
});
