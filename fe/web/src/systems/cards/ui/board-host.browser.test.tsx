import { render } from '@testing-library/react';
import { page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { useState } from '../../../ui/state/public.ts';
import { createCardHost, createCardRegistry } from '../public.js';
import type { CardEntry } from '../registry.js';
import { CardHead } from './card-head.tsx';
import { BoardHost, type BoardHostItem } from './board-host.tsx';

declare module '../registry.js' {
  interface CardDataMap {
    boardScrollTerm: { type: 'board-scroll-term'; id: string; title: string | null };
  }
}

const entry: CardEntry = {
  type: 'board-scroll-term',
  component: ({ card }) => (
    <div className="term">
      <CardHead className="card-drag-handle" title={card.id} />
      <div className="term-body">{card.id}</div>
    </div>
  ),
  defaultSize: Object.freeze({ w: 12, h: 8, minW: 4, minH: 4 }),
  title: (card) => card.id,
  accessibleName: (card) => card.id,
  create: Object.freeze({ mode: 'kernel-minted-only' as const }),
};

afterEach(() => { document.body.replaceChildren(); });

it('brings a newly selected card into the board viewport', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry();
  registry.register(entry);
  const host = createCardHost(registry);
  const items: readonly BoardHostItem[] = Array.from({ length: 5 }, (_, index) => Object.freeze({
    card: { type: 'board-scroll-term' as const, id: `card-${index}`, title: `Worker ${index}` },
    title: `Worker ${index}`,
    originalIndex: index,
  }));

  function Harness() {
    const [active, setActive] = useState('card-0');
    return (
      <>
        <button type="button" onClick={() => { setActive('card-4'); }}>Select last worker</button>
        <div style={{ display: 'flex', inlineSize: 900, blockSize: 360 }}>
          <BoardHost host={host} items={items} activeCardId={active} visible />
        </div>
      </>
    );
  }

  render(<Harness />);
  await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  const board = document.querySelector<HTMLElement>('[data-nc-card-board]')!;
  expect(board.scrollTop).toBe(0);

  await userEvent.click(document.querySelector<HTMLButtonElement>('button')!);
  await new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

  const selected = document.querySelector<HTMLElement>('[data-nc-card-id="card-4"]')!;
  const boardBox = board.getBoundingClientRect();
  const selectedBox = selected.getBoundingClientRect();
  expect(board.scrollTop, JSON.stringify({
    scrollHeight: board.scrollHeight,
    clientHeight: board.clientHeight,
    boardBox: { top: boardBox.top, bottom: boardBox.bottom },
    selectedBox: { top: selectedBox.top, bottom: selectedBox.bottom },
  })).toBeGreaterThan(0);
  expect(selectedBox.top).toBeLessThan(boardBox.bottom);
  expect(selectedBox.bottom).toBeGreaterThan(boardBox.top);
});
