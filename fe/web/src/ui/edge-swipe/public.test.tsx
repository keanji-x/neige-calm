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

it('discards an old touch at the disabled commit and admits a new enabled down', () => {
  const open = vi.fn(); const view = render(<EdgeSwipe enabled onSwipe={open} />);
  const edge = view.container.firstElementChild!;
  const pointer = (kind: string, x: number) => {
    const event = new Event(kind, { bubbles: true });
    Object.defineProperties(event, { pointerId: { value: 7 }, pointerType: { value: 'touch' }, isPrimary: { value: true }, clientX: { value: x }, clientY: { value: 100 } });
    fireEvent(edge, event);
  };
  pointer('pointerdown', 4);
  view.rerender(<EdgeSwipe enabled={false} onSwipe={open} />);
  view.rerender(<EdgeSwipe enabled onSwipe={open} />);
  pointer('pointerup', 100);
  expect(open).not.toHaveBeenCalled();
  pointer('pointerdown', 4); pointer('pointerup', 100);
  expect(open).toHaveBeenCalledOnce();
});

it('keeps cancel, pointer identity, admission and exact threshold controls', () => {
  const open = vi.fn(); const view = render(<EdgeSwipe enabled onSwipe={open} />);
  const edge = view.container.firstElementChild!;
  const pointer = (kind: string, x: number, y = 100, id = 1, type = 'touch', primary = true) => {
    const event = new Event(kind, { bubbles: true });
    Object.defineProperties(event, { pointerId: { value: id }, pointerType: { value: type }, isPrimary: { value: primary }, clientX: { value: x }, clientY: { value: y } });
    fireEvent(edge, event);
  };
  pointer('pointerdown', 4); pointer('pointercancel', 4); pointer('pointerup', 100);
  pointer('pointerdown', 4); pointer('pointerup', 100, 100, 2);
  pointer('pointerdown', 4, 100, 1, 'touch', false); pointer('pointerup', 100);
  pointer('pointerdown', 4, 100, 1, 'mouse'); pointer('pointerup', 100);
  pointer('pointerdown', 4); pointer('pointerup', 67);
  pointer('pointerdown', 4); pointer('pointerup', 100, 141);
  pointer('pointerdown', 4); pointer('pointerdown', 80); pointer('pointerup', 100);
  expect(open).not.toHaveBeenCalled();
  pointer('pointerdown', 4); pointer('pointerup', 68, 140);
  expect(open).toHaveBeenCalledOnce();
  pointer('pointerup', 100); expect(open).toHaveBeenCalledOnce();
});
