import { expect, it } from 'vitest';
import { springTrajectory } from './spring.ts';

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
