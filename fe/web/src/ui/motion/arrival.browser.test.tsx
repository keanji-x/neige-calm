import { StrictMode } from 'react';
import { cleanup, render } from '@testing-library/react';
import { commands } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import { useSpringArrival } from './arrival.ts';

function paint(value: number): Keyframe { return { opacity: value }; }
function Scene({ first = true, second = false, bindFirst = true }: { first?: boolean; second?: boolean; bindFirst?: boolean }) {
  const arrive = useSpringArrival(paint);
  return <>{first && <div data-testid="first" ref={bindFirst ? arrive : undefined}>First</div>}
    {second && <div data-testid="second" ref={arrive}>Second</div>}</>;
}
function pause(element: HTMLElement) {
  const animation = element.getAnimations()[0];
  expect(animation).toBeDefined();
  animation.pause();
  animation.currentTime = Number(animation.effect!.getTiming().duration) / 4;
  return animation;
}
afterEach(async () => { cleanup(); vi.restoreAllMocks(); await commands.emulateReducedMotion(false); });

it('lets a connected surface finish after ref withdrawal and does not replay that physical node', async () => {
  const view = render(<Scene />);
  const first = view.getByTestId('first');
  const animation = pause(first);
  const cancel = vi.spyOn(animation, 'cancel');
  view.rerender(<Scene bindFirst={false} />);
  expect(first.isConnected).toBe(true);
  expect(first.getAnimations()).toContain(animation);
  animation.finish();
  await expect.poll(() => first.getAnimations().length).toBe(0);
  expect(cancel).toHaveBeenCalledOnce();
  view.rerender(<Scene />);
  expect(view.getByTestId('first')).toBe(first);
  expect(first.getAnimations()).toHaveLength(0);
  view.unmount();
  // Finishing removes the active resource; owner cleanup has nothing to cancel twice.
  expect(cancel).toHaveBeenCalledOnce();
});

it('owns concurrent new surfaces independently and cancels only the surface actually removed', () => {
  const view = render(<Scene />);
  const first = view.getByTestId('first');
  const firstAnimation = pause(first);
  view.rerender(<Scene bindFirst={false} second />);
  const second = view.getByTestId('second');
  const secondAnimation = pause(second);
  expect(first.getAnimations()).toContain(firstAnimation);
  expect(secondAnimation).not.toBe(firstAnimation);
  view.rerender(<Scene first={false} second />);
  expect(first.isConnected).toBe(false);
  expect(first.getAnimations()).toHaveLength(0);
  expect(second.getAnimations()).toContain(secondAnimation);
  view.unmount();
  expect(second.getAnimations()).toHaveLength(0);
});

it('releases overlapping effects under reduced motion and skips existing surfaces when motion is reenabled', async () => {
  const view = render(<Scene second />);
  const first = view.getByTestId('first');
  const second = view.getByTestId('second');
  pause(first); pause(second);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => first.getAnimations().length + second.getAnimations().length).toBe(0);
  await commands.emulateReducedMotion(false);
  view.rerender(<Scene bindFirst={false} second />);
  view.rerender(<Scene second />);
  expect(first.getAnimations()).toHaveLength(0);
  expect(second.getAnimations()).toHaveLength(0);
  view.unmount();
  await commands.emulateReducedMotion(true);
  const reduced = render(<Scene />);
  expect(reduced.getByTestId('first').getAnimations()).toHaveLength(0);
});

it('keeps one live arrival through a StrictMode mount replay and releases it on unmount', () => {
  const view = render(<StrictMode><Scene /></StrictMode>);
  const first = view.getByTestId('first');
  expect(first.getAnimations()).toHaveLength(1);
  view.unmount();
  expect(first.getAnimations()).toHaveLength(0);
});
