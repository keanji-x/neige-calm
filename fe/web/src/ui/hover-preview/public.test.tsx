import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { HoverPreview } from './public.tsx';

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });
function mount() {
  return render(<HoverPreview title="Notes" trigger={activate => <button onClick={activate}>Notes</button>}><button>Action</button></HoverPreview>);
}
function advance(ms: number) { act(() => { vi.advanceTimersByTime(ms); }); }

it('keeps unavailable jsdom geometry in preparation without public identity or synthetic activation', () => {
  mount(); fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
  const card = screen.getByRole('dialog', { hidden: true });
  expect(card.getAttribute('aria-hidden')).toBe('true');
  expect(card.hasAttribute('data-nc-link-preview')).toBe(false);
  expect(card.hasAttribute('data-nc-escape-layer')).toBe(false);
  const action = vi.fn(); card.querySelector('button')!.addEventListener('click', action);
  fireEvent.click(card.querySelector('button')!); expect(action).not.toHaveBeenCalled();
  const escape = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
  fireEvent(document, escape); expect(escape.defaultPrevented).toBe(false);
  fireEvent.keyDown(screen.getByRole('button', { name: 'Notes' }), { key: 'Escape' });
  expect(card.isConnected).toBe(false);
});
it('cancels an accidental hover before opening', () => {
  mount(); fireEvent.pointerEnter(screen.getByRole('button', { name: 'Notes' }).parentElement!); advance(100);
  fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!); advance(2000);
  expect(screen.queryByRole('dialog', { hidden: true })).toBeNull();
});
it('disposes unavailable preparation on outside press and unmount without waiting for completion', () => {
  const view = mount(); fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
  expect(screen.getByRole('dialog', { hidden: true })).toBeTruthy();
  fireEvent.pointerDown(document.body); expect(screen.queryByRole('dialog', { hidden: true })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Notes' })); view.unmount(); advance(3000);
  expect(screen.queryByRole('dialog', { hidden: true })).toBeNull(); expect(vi.getTimerCount()).toBe(0);
});

it('cancels a preparing hover request when its actual trigger is left', () => {
  mount(); const trigger = screen.getByRole('button', { name: 'Notes' }).parentElement!;
  fireEvent.pointerEnter(trigger); advance(300);
  expect(screen.getByRole('dialog', { hidden: true })).toBeTruthy();
  fireEvent.pointerLeave(trigger); advance(180);
  expect(screen.queryByRole('dialog', { hidden: true })).toBeNull();
});
