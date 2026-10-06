import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';
import '../../styles/entry.css';
import { SpringRotation } from './rotation.tsx';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));

it('mounts at the declared angle and preserves momentum through reversal', async () => {
  const view = render(<SpringRotation angle={0} className="">Marker</SpringRotation>);
  const marker = view.container.firstElementChild as HTMLElement;
  expect(marker.getAnimations()).toHaveLength(0);
  view.rerender(<SpringRotation angle={90} className="">Marker</SpringRotation>);
  const outward = marker.getAnimations()[0];
  expect((outward.effect as KeyframeEffect).getKeyframes().length).toBeGreaterThan(2);
  outward.pause(); outward.currentTime = 80;
  const painted = parseFloat(getComputedStyle(marker).rotate);
  view.rerender(<SpringRotation angle={0} className="">Marker</SpringRotation>);
  const reverse = marker.getAnimations()[0];
  const frames = (reverse.effect as KeyframeEffect).getKeyframes();
  expect(parseFloat(String(frames[0].rotate))).toBeCloseTo(painted, 1);
  expect(parseFloat(String(frames[1].rotate))).toBeGreaterThan(parseFloat(String(frames[0].rotate)));
  await reverse.finished;
  await frame();
  expect(getComputedStyle(marker).rotate).toBe('0deg');
  expect(marker.getAnimations()).toHaveLength(0);
});

it('shows an initially expanded angle directly and cancels travel under reduced motion', async () => {
  const view = render(<SpringRotation angle={90} className="">Marker</SpringRotation>);
  const marker = view.container.firstElementChild as HTMLElement;
  expect(marker.getAnimations()).toHaveLength(0);
  expect(getComputedStyle(marker).rotate).toBe('90deg');
  view.rerender(<SpringRotation angle={0} className="">Marker</SpringRotation>);
  expect(marker.getAnimations()).toHaveLength(1);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => marker.getAnimations()).toHaveLength(0);
  expect(getComputedStyle(marker).rotate).toBe('0deg');
});
