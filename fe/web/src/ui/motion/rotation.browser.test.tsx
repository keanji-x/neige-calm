import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';
import '../../styles/entry.css';
import { springTrajectory } from './spring.ts';
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
  outward.startTime = document.timeline.currentTime; outward.currentTime = 80;
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

it.each([-1, 0.5, 0])('retargets the real rotation hook with native wall-clock momentum %s', async rate => {
  const view = render(<SpringRotation angle={0} className="">Marker</SpringRotation>);
  const marker = view.container.firstElementChild as HTMLElement;
  view.rerender(<SpringRotation angle={90} className="">Marker</SpringRotation>);
  const outward = marker.getAnimations()[0]; await outward.ready;
  outward.playbackRate = rate; outward.currentTime = 80;
  const raw = springTrajectory(0, 90, 0).sample(80);
  view.rerender(<SpringRotation angle={0} className="">Marker</SpringRotation>);
  const reverse = marker.getAnimations()[0];
  const keyframes = (reverse.effect as KeyframeEffect).getKeyframes();
  const expected = springTrajectory(raw.value, 0, raw.velocity * rate);
  expect(parseFloat(String(keyframes[0].rotate))).toBeCloseTo(raw.value, 3);
  const time = expected.duration / (keyframes.length - 1);
  expect(parseFloat(String(keyframes[1].rotate))).toBeCloseTo(expected.sample(time).value, 3);
  view.unmount();
});
