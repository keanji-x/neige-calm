// @vitest-environment jsdom

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

import {
  BoardHost, createCardHost, createCardRegistry, registerAvailableBuiltinCards,
  type BoardHostItem, type RegisteredCard,
} from '../public.js';

beforeEach(() => {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function boardOfBuiltins(onRemoveCard: (cardId: string) => void) {
  const registry = createCardRegistry();
  registerAvailableBuiltinCards(registry);
  const host = createCardHost(registry);
  const resolve = (wire: { id: string; kind: string; payload: unknown }): RegisteredCard => {
    const card = registry.resolve(wire);
    if (card === null) throw new Error(`no built-in entry claims ${wire.kind}`);
    return card;
  };
  const items: readonly BoardHostItem[] = [
    Object.freeze({
      card: resolve({ id: 'card-term', kind: 'terminal', payload: {} }),
      title: 'Build log',
      originalIndex: 0,
      deletable: true,
      activity: null,
    }),
    Object.freeze({
      card: resolve({ id: 'card-file', kind: 'file-viewer', payload: { path: '/repo/notes.md' } }),
      title: 'Notes',
      originalIndex: 1,
      deletable: true,
      activity: null,
    }),
  ];
  return render(
    <BoardHost host={host} items={items} activeCardId="card-term" visible onRemoveCard={onRemoveCard} />,
  );
}

it('draws the delete on a real terminal card and calls back with that card id', async () => {
  const onRemoveCard = vi.fn();
  boardOfBuiltins(onRemoveCard);
  expect(screen.getByText('No terminal session available.')).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Delete card Build log' }));
  expect(onRemoveCard).toHaveBeenCalledWith('card-term');
});

it('draws the delete on a real file card and calls back with that card id', async () => {
  const onRemoveCard = vi.fn();
  boardOfBuiltins(onRemoveCard);
  expect(screen.getByText('This board was built without filesystem access.')).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Delete card Notes' }));
  expect(onRemoveCard).toHaveBeenCalledWith('card-file');
});
