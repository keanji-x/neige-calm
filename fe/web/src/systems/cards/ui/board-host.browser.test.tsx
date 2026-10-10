import { cleanup, act, render } from '@testing-library/react';
import { commands, page as browserPage, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';

import { useState } from '../../../ui/state/public.ts';
import { createCardHost, createCardRegistry } from '../public.js';
import type { CardEntry } from '../registry.js';
import { readMotionTransition } from '../../../ui/motion/transition.ts';
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

afterEach(async () => { cleanup(); document.body.replaceChildren(); await commands.emulateReducedMotion(false); });

it('brings a newly selected card into the board viewport', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry();
  registry.register(entry);
  const host = createCardHost(registry);
  const items: readonly BoardHostItem[] = Array.from({ length: 5 }, (_, index) => Object.freeze({
    card: { type: 'board-scroll-term' as const, id: `card-${index}`, title: `Worker ${index}` },
    title: `Worker ${index}`,
    originalIndex: index,
    activity: null, notice: null,
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

it('keeps the real dragged card under direct pointer control', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const items: readonly BoardHostItem[] = [{ card: { type: 'board-scroll-term', id: 'direct-card', title: 'Direct card' }, title: 'Direct card', originalIndex: 0, activity: null, notice: null }];
  render(<div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}><BoardHost host={host} items={items} activeCardId="direct-card" visible /></div>);
  await expect.poll(() => document.querySelector('[data-nc-card-id="direct-card"]')).not.toBeNull();
  const handle = document.querySelector<HTMLElement>('[data-nc-card-id="direct-card"] [data-nc-card-drag]')!;
  const box = handle.getBoundingClientRect();
  const point = { clientX: box.left + 20, clientY: box.top + 10 };
  try {
    act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
    act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX, clientY: point.clientY + 80 })); });
    const dragged = document.querySelector<HTMLElement>('[data-nc-card-id="direct-card"]')!;
    expect(dragged).not.toBeNull();
    expect(dragged.classList.contains('react-draggable-dragging')).toBe(true);
    expect(getComputedStyle(dragged).transitionProperty).toBe('none');
    expect(dragged.getAnimations()).toHaveLength(0);
  } finally {
    act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX, clientY: point.clientY + 80 })); });
  }
});

it('keeps feedback timing at rest and suppresses motion during a real resize', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const items: readonly BoardHostItem[] = [{ card: { type: 'board-scroll-term', id: 'resize-card', title: 'Resize card' }, title: 'Resize card', originalIndex: 0, activity: null, notice: null }];
  render(<div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}><BoardHost host={host} items={items} activeCardId="resize-card" visible /></div>);
  await expect.poll(() => document.querySelector('[data-nc-card-id="resize-card"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="resize-card"]')!;
  const motion = readMotionTransition(cell, 'feedback');
  expect(parseFloat(getComputedStyle(cell).transitionDuration)).toBe(motion.duration);
  const handle = cell.querySelector<HTMLElement>('[data-nc-card-resize="se"]')!;
  const box = handle.getBoundingClientRect();
  const point = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 };
  try {
    act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
    act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX + 40, clientY: point.clientY + 40 })); });
    expect(cell.classList.contains('resizing')).toBe(true);
    expect(getComputedStyle(cell).transitionProperty).toBe('none');
    expect(cell.getAnimations()).toHaveLength(0);
  } finally {
    act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX + 40, clientY: point.clientY + 40 })); });
  }
});

it('settles a remaining card with native spring geometry after compaction', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const item = (id: string, originalIndex: number): BoardHostItem => ({
    card: { type: 'board-scroll-term', id, title: id }, title: id, originalIndex, activity: null, notice: null,
  });
  const first = item('first', 0), second = item('second', 1);
  const scene = (items: readonly BoardHostItem[]) => <div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}>
    <BoardHost host={host} items={items} activeCardId={null} visible />
  </div>;
  const view = render(scene([first, second]));
  await expect.poll(() => document.querySelector('[data-nc-card-id="second"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="second"]')!;
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  const before = cell.getBoundingClientRect().top;
  view.rerender(scene([second]));
  const geometry = () => cell.getAnimations().find(animation =>
    (animation.effect as KeyframeEffect).getKeyframes().some(frame => frame.transform !== undefined));
  await expect.poll(geometry).toBeDefined();
  const frames = (geometry()!.effect as KeyframeEffect).getKeyframes();
  expect(frames.length).toBeGreaterThan(2);
  expect(cell.isConnected).toBe(true);
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  expect(cell.getBoundingClientRect().top).toBeLessThan(before);
});

it('hands an in-flight compaction to the pointer without jumping', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const item = (id: string, originalIndex: number): BoardHostItem => ({
    card: { type: 'board-scroll-term', id, title: id }, title: id, originalIndex, activity: null, notice: null,
  });
  const first = item('handoff-first', 0), second = item('handoff-second', 1);
  const scene = (items: readonly BoardHostItem[]) => <div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}>
    <BoardHost host={host} items={items} activeCardId={null} visible />
  </div>;
  const view = render(scene([first, second]));
  await expect.poll(() => document.querySelector('[data-nc-card-id="handoff-second"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="handoff-second"]')!;
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  view.rerender(scene([second]));
  const animation = cell.getAnimations().find(animation =>
    (animation.effect as KeyframeEffect).getKeyframes().some(frame => frame.transform !== undefined))!;
  expect(animation).toBeDefined();
  animation.pause(); animation.currentTime = 80;
  const before = cell.getBoundingClientRect();
  const handle = cell.querySelector<HTMLElement>('[data-nc-card-drag]')!;
  const box = handle.getBoundingClientRect();
  const point = { clientX: box.left + 20, clientY: box.top + 10 };
  try {
    act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
    act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX, clientY: point.clientY + 80 })); });
    expect(cell.getAnimations()).toHaveLength(0);
    expect(cell.getBoundingClientRect().top).toBeCloseTo(before.top + 80, 0);
  } finally {
    act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX, clientY: point.clientY + 80 })); });
  }
});

it('compacts directly when reduced motion is enabled', async () => {
  await commands.emulateReducedMotion(true);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const item = (id: string, originalIndex: number): BoardHostItem => ({
    card: { type: 'board-scroll-term', id, title: id }, title: id, originalIndex, activity: null, notice: null,
  });
  const first = item('reduce-first', 0), second = item('reduce-second', 1);
  const scene = (items: readonly BoardHostItem[]) => <div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}>
    <BoardHost host={host} items={items} activeCardId={null} visible />
  </div>;
  const view = render(scene([first, second]));
  await expect.poll(() => document.querySelector('[data-nc-card-id="reduce-second"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="reduce-second"]')!;
  const before = cell.getBoundingClientRect().top;
  view.rerender(scene([second]));
  expect(cell.getAnimations()).toHaveLength(0);
  expect(cell.getBoundingClientRect().top).toBeLessThan(before);
});

it('holds the painted position while an in-flight compaction is resized', async () => {
  await browserPage.viewport(1200, 800);
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const item = (id: string, originalIndex: number): BoardHostItem => ({
    card: { type: 'board-scroll-term', id, title: id }, title: id, originalIndex, activity: null, notice: null,
  });
  const first = item('resize-flight-first', 0), second = item('resize-flight-second', 1);
  const scene = (items: readonly BoardHostItem[]) => <div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}>
    <BoardHost host={host} items={items} activeCardId={null} visible />
  </div>;
  const view = render(scene([first, second]));
  await expect.poll(() => document.querySelector('[data-nc-card-id="resize-flight-second"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="resize-flight-second"]')!;
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  view.rerender(scene([second]));
  const animation = cell.getAnimations().find(animation =>
    (animation.effect as KeyframeEffect).getKeyframes().some(frame => frame.transform !== undefined))!;
  expect(animation).toBeDefined();
  animation.pause(); animation.currentTime = 80;
  const before = cell.getBoundingClientRect();
  const handle = cell.querySelector<HTMLElement>('[data-nc-card-resize="se"]')!;
  const box = handle.getBoundingClientRect();
  const point = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 };
  try {
    act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
    expect(cell.getAnimations()).toHaveLength(0);
    expect(cell.getBoundingClientRect().top).toBeCloseTo(before.top, 0);
    act(() => { document.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, buttons: 1, clientX: point.clientX, clientY: point.clientY + 80 })); });
    expect(cell.getBoundingClientRect().top).toBeCloseTo(before.top, 0);
    expect(cell.getBoundingClientRect().height).toBeGreaterThan(before.height);
  } finally {
    act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, clientX: point.clientX, clientY: point.clientY + 80 })); });
  }
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  expect(cell.getBoundingClientRect().top).toBeLessThan(before.top);
});

it('keeps logical placement when a moving header is clicked without dragging', async () => {
  const registry = createCardRegistry(); registry.register(entry);
  const host = createCardHost(registry);
  const items: readonly BoardHostItem[] = Array.from({ length: 4 }, (_, originalIndex) => ({
    card: { type: 'board-scroll-term', id: `click-${originalIndex}`, title: null },
    title: `click-${originalIndex}`, originalIndex, activity: null, notice: null,
  }));
  const scene = (items: readonly BoardHostItem[]) => <div style={{ display: 'flex', inlineSize: 900, blockSize: 600 }}>
    <BoardHost host={host} items={items} activeCardId={null} visible />
  </div>;
  const view = render(scene(items));
  await expect.poll(() => document.querySelector('[data-nc-card-id="click-2"]')).not.toBeNull();
  const cell = document.querySelector<HTMLElement>('[data-nc-card-id="click-2"]')!;
  view.rerender(scene(items.slice(2)));
  const animation = cell.getAnimations().find(animation =>
    (animation.effect as KeyframeEffect).getKeyframes().some(frame => frame.transform !== undefined))!;
  expect(animation).toBeDefined();
  animation.pause(); animation.currentTime = 80;
  const goal = cell.style.getPropertyValue('--nc-card-layout-y');
  const handle = cell.querySelector<HTMLElement>('[data-nc-card-drag]')!;
  const box = handle.getBoundingClientRect();
  const point = { clientX: box.left + 20, clientY: box.top + 10 };
  act(() => { handle.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button: 0, buttons: 1, ...point })); });
  act(() => { document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button: 0, ...point })); });
  await expect.poll(() => cell.getAnimations().length).toBe(0);
  expect(cell.style.getPropertyValue('--nc-card-layout-y')).toBe(goal);
  const next = document.querySelector<HTMLElement>('[data-nc-card-id="click-3"]')!;
  expect(next.getBoundingClientRect().top).toBeGreaterThan(cell.getBoundingClientRect().top);
});
