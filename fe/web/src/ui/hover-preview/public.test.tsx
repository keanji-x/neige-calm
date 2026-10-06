import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HoverPreview } from './public.tsx';

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });
function mount() {
  return render(<HoverPreview title="Notes" trigger={(pin) => <button onClick={pin}>Notes</button>}><p>Preview body</p></HoverPreview>);
}
function advance(ms: number) { act(() => { vi.advanceTimersByTime(ms); }); }
function hover() { fireEvent.pointerEnter(screen.getByRole('button', { name: 'Notes' }).parentElement!); }

describe('HoverPreview lifecycle', () => {
  it('delays preview, then pins after an uninterrupted dwell', () => {
    mount(); hover(); advance(299);
    expect(screen.queryByRole('dialog')).toBeNull();
    advance(1);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-pinned')).toBe(false);
    advance(1000);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-pinned')).toBe(true);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(2000);
    expect(screen.getByRole('dialog')).toBeTruthy();
  });
  it('does not pin after the pointer leaves just before the dwell deadline', () => {
    mount(); hover(); advance(1200);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-pinned')).toBe(false);
    advance(80);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('allows pointer transfer into the card before the leave grace ends', () => {
    mount(); hover(); advance(300);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    fireEvent.pointerEnter(screen.getByRole('dialog'));
    advance(1000);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-pinned')).toBe(true);
  });
  it('pins by keyboard, restores focus on close, and does not reopen', () => {
    mount();
    const trigger = screen.getByRole('button', { name: 'Notes' });
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.getByRole('dialog').hasAttribute('data-nc-pinned')).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Close preview' }));
    advance(2000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
  it('Escape closes only the topmost card', () => {
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
    render(<HoverPreview title="Second" trigger={(pin) => <button onClick={pin}>Second</button>}>Second body</HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Second' }));
    fireEvent.keyDown(screen.getByRole('button', { name: 'Notes' }), { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Preview: Second' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Preview: Notes' })).toBeTruthy();
  });
  it('unmount disposes all timers and its portal', () => {
    const view = mount(); hover(); advance(300);
    view.unmount(); advance(3000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });
});
