import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { EdgeSwipe } from './public.tsx';

afterEach(cleanup);
it('recognizes a horizontal primary touch swipe while leaving mouse and vertical gestures alone', () => {
  const open = vi.fn();
  const view = render(<EdgeSwipe enabled onSwipe={open} />);
  const edge = view.container.firstElementChild!;
  const pointer = (kind: string, x: number, y: number, type: string) => {
    const event = new Event(kind, { bubbles: true });
    Object.defineProperties(event, { pointerId: { value: 1 }, pointerType: { value: type }, isPrimary: { value: true }, clientX: { value: x }, clientY: { value: y } });
    fireEvent(edge, event);
  };
  pointer('pointerdown', 4, 100, 'mouse'); pointer('pointerup', 100, 100, 'mouse');
  pointer('pointerdown', 4, 100, 'touch'); pointer('pointerup', 100, 200, 'touch');
  expect(open).not.toHaveBeenCalled();
  pointer('pointerdown', 4, 100, 'touch'); pointer('pointerup', 100, 100, 'touch');
  expect(open).toHaveBeenCalledTimes(1);
  view.rerender(<EdgeSwipe enabled={false} onSwipe={open} />);
  pointer('pointerdown', 4, 100, 'touch'); pointer('pointerup', 100, 100, 'touch');
  expect(open).toHaveBeenCalledTimes(1);
});
