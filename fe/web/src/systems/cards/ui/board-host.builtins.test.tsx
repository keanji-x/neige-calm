// @vitest-environment jsdom
//
// The head's × on the cards that actually ship.
//
// `board-host.test.tsx` proves the *board* hands a component an `onRemove`
// bound to its own card id, and it does so against a fixture entry that draws
// its own `CardHead`. That fixture is the gap this file closes: a built-in that
// forgets to pass `onRemove` down — or drops `onClose` from its head — loses the
// × on every card of that kind while the whole suite stays green, because no
// test mounted the real component. So here the registry is the production one
// (`registerAvailableBuiltinCards`), the cards come out of the real
// `fromKernel`, and nothing between the click and the callback is stubbed.
//
// The two subjects are the two kinds whose heads own a delete: the PTY head
// shared by terminal / codex / claude (`terminal-card.tsx`) and the file card's
// own (`builtins/file-viewer.tsx`). Both are mounted without their backing
// runtime — a `terminal_id`-less row has no session, and a host with no
// filesystem port says so — which is exactly what keeps the assertion about the
// head rather than about xterm or CodeMirror.

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
    }),
    Object.freeze({
      card: resolve({ id: 'card-file', kind: 'file-viewer', payload: { path: '/repo/notes.md' } }),
      title: 'Notes',
      originalIndex: 1,
      deletable: true,
    }),
  ];
  return render(
    <BoardHost host={host} items={items} activeCardId="card-term" visible onRemoveCard={onRemoveCard} />,
  );
}

it('draws the delete on a real terminal card and calls back with that card id', async () => {
  const onRemoveCard = vi.fn();
  boardOfBuiltins(onRemoveCard);
  // The head is the production one: the card is `terminal_id`-less, so the body
  // is the unavailable-session line and the × is the only control on it.
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
