import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { HoverPreview } from './public.tsx';

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });
function mount() {
  return render(<HoverPreview title="Notes" trigger={(activate) => <button onClick={activate}>Notes</button>}><p>Preview body</p></HoverPreview>);
}
function advance(ms: number) { act(() => { vi.advanceTimersByTime(ms); }); }
function hover() { fireEvent.pointerEnter(screen.getByRole('button', { name: 'Notes' }).parentElement!); }

describe('HoverPreview lifecycle', () => {
  it('delays preview, becomes ready, then dismisses when the pointer leaves', () => {
    mount(); hover(); advance(299);
    expect(screen.queryByRole('dialog')).toBeNull();
    advance(1);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-ready')).toBe(false);
    advance(1000);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-ready')).toBe(true);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(2000);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('does not become ready after the pointer leaves just before the dwell deadline', () => {
    mount(); hover(); advance(1200);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-ready')).toBe(false);
    advance(80);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('allows pointer transfer into the card before the leave grace ends', () => {
    mount(); hover(); advance(300);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    fireEvent.pointerEnter(screen.getByRole('dialog'));
    advance(1000);
    expect(screen.getByRole('dialog').hasAttribute('data-nc-ready')).toBe(true);
  });
  it('keeps a ready card while entering it, then closes when leaving the card', () => {
    mount(); hover(); advance(1300);
    fireEvent.pointerLeave(screen.getByRole('button', { name: 'Notes' }).parentElement!);
    advance(100);
    fireEvent.pointerEnter(screen.getByRole('dialog'));
    advance(2000);
    expect(screen.getByRole('dialog')).toBeTruthy();
    fireEvent.pointerLeave(screen.getByRole('dialog'));
    advance(180);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('enables interaction by keyboard, restores focus on close, and does not reopen', () => {
    mount();
    const trigger = screen.getByRole('button', { name: 'Notes' });
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(screen.getByRole('dialog').hasAttribute('data-nc-ready')).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Close preview' }));
    advance(2000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
  it('Escape closes only the topmost card', () => {
    mount();
    fireEvent.click(screen.getByRole('button', { name: 'Notes' }));
    render(<HoverPreview title="Second" trigger={(activate) => <button onClick={activate}>Second</button>}>Second body</HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Second' }));
    fireEvent.keyDown(screen.getByRole('button', { name: 'Notes' }), { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: 'Preview: Second' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Preview: Notes' })).toBeTruthy();
  });
  it('keeps nested preview scrolling open and dismisses only when its anchor scrolls', () => {
    render(<HoverPreview title="Parent" trigger={(activate) => <button onClick={activate}>Parent</button>}>
      <HoverPreview title="Child" trigger={(activate) => <button onClick={activate}>Child</button>}>
        <div data-testid="child-scroll">Long child contents</div>
      </HoverPreview>
    </HoverPreview>);
    fireEvent.click(screen.getByRole('button', { name: 'Parent' }));
    fireEvent.click(screen.getByRole('button', { name: 'Child' }));
    fireEvent.scroll(screen.getByTestId('child-scroll'));
    expect(screen.getAllByRole('dialog')).toHaveLength(2);
    const parent = screen.getByRole('dialog', { name: 'Preview: Parent' });
    fireEvent.scroll(parent.children[1]!);
    expect(screen.queryByRole('dialog', { name: 'Preview: Child' })).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Preview: Parent' })).toBeTruthy();
    fireEvent.scroll(document);
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it('unmount disposes all timers and its portal', () => {
    const view = mount(); hover(); advance(300);
    view.unmount(); advance(3000);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });
});
