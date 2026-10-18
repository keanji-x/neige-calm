import { expect, it } from 'vitest';
import { playSpring, springTrajectory } from './spring.ts';

it('acquires paired native controls on the owning document timeline in the DOM platform model', async () => {
  const elements = [document.createElement('div'), document.createElement('div')];
  const timeline = document.timeline;
  expect(timeline).toBeDefined();
  expect(document.timeline).toBe(timeline);
  expect(typeof timeline.currentTime).toBe('number');
  const playback = playSpring(elements, 0, 1, 0, value => ({ opacity: value }));
  const [first, second] = playback.animations;
  expect(first.timeline).toBe(timeline);
  expect(second.timeline).toBe(timeline);
  expect(first.startTime).toBe(timeline.currentTime);
  expect(second.startTime).toBe(first.startTime);
  const ready = first.ready;
  const finished = first.finished;
  first.pause(); first.currentTime = 80; first.updatePlaybackRate(0.5); first.play();
  expect(await first.ready).toBe(first);
  expect(first.finished).toBe(finished);
  expect(await ready).toBe(first);
  expect(playback.sample()).toEqual(springTrajectory(0, 1, 0).sample(80));
  first.finish(); second.finish();
  expect(await finished).toBe(first);
  await playback.finished;
});

it('keeps inactive document timeline identity separate in the DOM platform model', async () => {
  const inactive = document.implementation.createHTMLDocument();
  expect(inactive.timeline).not.toBe(document.timeline);
  expect(inactive.timeline).toBe(inactive.timeline);
  expect(inactive.timeline.currentTime).toBeNull();
  const animation = inactive.createElement('div').animate([], { duration: 100 });
  expect(animation.timeline).toBe(inactive.timeline);
  const cancelled = expect(animation.finished).rejects.toMatchObject({ name: 'AbortError' });
  animation.cancel();
  await cancelled;
});

it('uses the same normalized physics for short and large travel', () => {
  const small = springTrajectory(0, 1, 0);
  const large = springTrajectory(0, 1000, 0);
  for (const time of [25, 75, 150]) {
    expect(large.sample(time).value / 1000).toBeCloseTo(small.sample(time).value, 6);
    expect(large.sample(time).velocity / 1000).toBeCloseTo(small.sample(time).velocity, 6);
  }
});

it('inherits current position and analytical velocity when the goal reverses', () => {
  const outward = springTrajectory(40, 500, 0);
  const state = outward.sample(80);
  const reverse = springTrajectory(state.value, 40, state.velocity);
  expect(reverse.sample(0).value).toBeCloseTo(state.value, 6);
  expect(reverse.sample(0).velocity).toBeCloseTo(state.velocity, 6);
  expect(reverse.sample(10).value).toBeGreaterThan(state.value);
  expect(reverse.sample(reverse.duration)).toEqual({ value: 40, velocity: 0 });
});

it.each([NaN, Infinity, -Infinity])('rejects invalid spring state %s', invalid => {
  expect(() => springTrajectory(invalid, 40, 0)).toThrow('Invalid spring state');
  expect(() => springTrajectory(40, invalid, 0)).toThrow('Invalid spring state');
  expect(() => springTrajectory(40, 80, invalid)).toThrow('Invalid spring state');
});
