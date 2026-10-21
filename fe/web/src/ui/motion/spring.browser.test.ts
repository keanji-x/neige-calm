import { afterEach, expect, it, vi } from 'vitest';
import { playPointSpring, playSpring, springTrajectory } from './spring.ts';

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

it('rejects mixed documents before acquiring any native effect', () => {
  const primary = document.body.appendChild(document.createElement('div'));
  const foreign = document.implementation.createHTMLDocument().createElement('div');
  expect(() => playSpring([primary, foreign], 0, 1, 0, opacity => ({ opacity })))
    .toThrow('Spring surfaces must share a document');
  expect(primary.getAnimations()).toHaveLength(0);
});

it('discards acquired effects if a sibling surface cannot start', () => {
  const primary = document.body.appendChild(document.createElement('div'));
  const peer = document.body.appendChild(document.createElement('div'));
  vi.spyOn(peer, 'animate').mockImplementation(() => { throw new Error('Native start failed'); });
  expect(() => playSpring([primary, peer], 0, 1, 0, opacity => ({ opacity }))).toThrow('Native start failed');
  expect(primary.getAnimations()).toHaveLength(0);
});

it('starts paired surfaces on the same browser rendering frame', async () => {
  const primary = document.body.appendChild(document.createElement('div'));
  const peer = document.body.appendChild(document.createElement('div'));
  const playback = playSpring([primary, peer], 0, 1, 0, opacity => ({ opacity }));
  await Promise.all(playback.animations.map(animation => animation.ready));
  expect(playback.animations[0].startTime).not.toBeNull();
  expect(playback.animations[0].startTime).toBe(playback.animations[1].startTime);
  playback.cancel();
  await expect(playback.finished).rejects.toBeDefined();
});

it('samples both point axes on one native clock and preserves their reversal velocity', async () => {
  const element = document.body.appendChild(document.createElement('div'));
  const paint = (value: { x: number; y: number }) => ({ transform: `translate(${value.x}px, ${value.y}px)` });
  const outward = playPointSpring([element], { x: 0, y: 0 }, { x: 600, y: 30 }, { x: 0, y: 0 }, paint);
  outward.animations[0].startTime = document.timeline.currentTime; outward.animations[0].currentTime = 80;
  const state = outward.sample();
  expect(state.value.x / 600).toBeCloseTo(state.value.y / 30, 6);
  expect(state.velocity.x / 600).toBeCloseTo(state.velocity.y / 30, 6);
  outward.cancel(); await expect(outward.finished).rejects.toBeDefined();
  const reverse = playPointSpring([element], state.value, { x: 0, y: 0 }, state.velocity, paint);
  expect(reverse.sample().value.x).toBeCloseTo(state.value.x, 6);
  expect(reverse.sample().velocity.y).toBeCloseTo(state.velocity.y, 6);
  reverse.animations[0].currentTime = 10;
  expect(reverse.sample().value.x).toBeGreaterThan(state.value.x);
  expect(reverse.sample().value.y).toBeGreaterThan(state.value.y);
  await reverse.finished;
  expect(reverse.sample()).toEqual({ value: { x: 0, y: 0 }, velocity: { x: 0, y: 0 } });
  reverse.cancel();
});

const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
it('invalidates changing native time and the terminal sample without paused settled or cancelled pumping', async () => {
  const element = document.body.appendChild(document.createElement('div'));
  const request = vi.spyOn(window, 'requestAnimationFrame');
  const times: (number | null)[] = [];
  const trajectory = { scaleVelocity: (value: number, rate: number) => value * rate, duration: 500, sample: (time: number) => ({ value: time / 500, velocity: 0 }) };
  const { playTrajectory } = await import('./spring.ts');
  const playback = playTrajectory([element], trajectory, opacity => ({ opacity }), () => { times.push(playback.animations[0].currentTime as number | null); });
  void playback.finished.catch(() => {});
  await frames(3);
  expect(times.length).toBeGreaterThan(0);
  const animation = playback.animations[0]; animation.pause(); await animation.ready;
  await frames(3); const held = times.length; request.mockClear();
  await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(times).toHaveLength(held);
  animation.finish(); await playback.finished;
  expect(times.at(-1)).toBe(500); expect(times).toHaveLength(held + 1);
  request.mockClear(); await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(times).toHaveLength(held + 1);
  playback.cancel();
  const cancelled = vi.fn();
  const other = playTrajectory([element], trajectory, opacity => ({ opacity }), cancelled);
  void other.finished.catch(() => {}); other.cancel();
  request.mockClear(); await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(cancelled).not.toHaveBeenCalled();
});


it('rearms native play after a drained pause without replacing the effect or promises', async () => {
  const { playTrajectory } = await import('./spring.ts');
  const element = document.body.appendChild(document.createElement('div'));
  const notices = vi.fn();
  const playback = playTrajectory([element], { scaleVelocity: (value: number, rate: number) => value * rate, duration: 1000, sample: time => ({ value: time / 1000, velocity: 1 }) }, opacity => ({ opacity }), notices);
  void playback.finished.catch(() => {});
  const animation = playback.animations[0]; const effect = animation.effect; const finished = animation.finished;
  animation.pause(); await animation.ready; await frames(3); notices.mockClear();
  animation.play(); await animation.ready; await frames(4);
  expect(notices.mock.calls.length).toBeGreaterThan(1);
  expect(element.getAnimations()[0]).toBe(animation); expect(animation.effect).toBe(effect); expect(animation.finished).toBe(finished);
  playback.cancel();
});

it('publishes a paused native seek once then returns to zero work', async () => {
  const { playTrajectory } = await import('./spring.ts');
  const element = document.body.appendChild(document.createElement('div'));
  const notices = vi.fn(); const request = vi.spyOn(window, 'requestAnimationFrame');
  const playback = playTrajectory([element], { scaleVelocity: (value: number, rate: number) => value * rate, duration: 1000, sample: time => ({ value: time / 1000, velocity: 1 }) }, opacity => ({ opacity }), notices);
  void playback.finished.catch(() => {});
  const animation = element.getAnimations()[0]; animation.pause(); await animation.ready; await frames(3); notices.mockClear();
  animation.currentTime = 80; await frames(3);
  expect(notices).toHaveBeenCalledOnce(); expect(getComputedStyle(element).opacity).toBe('0.08');
  notices.mockClear(); request.mockClear(); await frames(20);
  expect(request).toHaveBeenCalledTimes(20); expect(notices).not.toHaveBeenCalled();
  playback.cancel();
});

// These controls are real browser Animations; sampling and retargeting use the production primitives.
it.each([-1, 0.5, 0])('reports scalar wall-clock velocity at native rate %s', async rate => {
  const element = document.body.appendChild(document.createElement('div'));
  const playback = playSpring([element], 0, 100, 0, value => ({ translate: `${value}px` }));
  void playback.finished.catch(() => {});
  const animation = playback.animations[0];
  await animation.ready;
  animation.playbackRate = rate; animation.currentTime = 80;
  const raw = springTrajectory(0, 100, 0).sample(80);
  expect(playback.sample().value).toBeCloseTo(raw.value, 8);
  expect(playback.sample().velocity).toBeCloseTo(raw.velocity * rate, 8);
  expect(parseFloat(getComputedStyle(element).translate)).toBeCloseTo(raw.value, 0);
  playback.cancel();
});

it.each([-1, 0.5, 0])('reports point wall-clock velocity at native rate %s', async rate => {
  const element = document.body.appendChild(document.createElement('div'));
  const playback = playPointSpring([element], { x: 0, y: 0 }, { x: 100, y: 30 }, { x: 0, y: 0 },
    value => ({ translate: `${value.x}px ${value.y}px` }));
  void playback.finished.catch(() => {});
  const animation = playback.animations[0]; await animation.ready;
  animation.playbackRate = rate; animation.currentTime = 80;
  const x = springTrajectory(0, 100, 0).sample(80); const y = springTrajectory(0, 30, 0).sample(80);
  const state = playback.sample();
  expect(state.value.x).toBeCloseTo(x.value, 8); expect(state.value.y).toBeCloseTo(y.value, 8);
  expect(state.velocity.x).toBeCloseTo(x.velocity * rate, 8); expect(state.velocity.y).toBeCloseTo(y.velocity * rate, 8);
  playback.cancel();
});

it('uses public clocks before and after pending ready and zeroes held and terminal velocity', async () => {
  const element = document.body.appendChild(document.createElement('div'));
  const playback = playSpring([element], 0, 100, 20, value => ({ translate: `${value}px` }));
  void playback.finished.catch(() => {});
  const animation = playback.animations[0];
  const trajectory = springTrajectory(0, 100, 20);
  const check = () => {
    // WAAPI can expose negative time (even rounding below zero on acquisition).
    // The native fill model holds the endpoint outside the trajectory interval.
    const raw = trajectory.sample(Math.max(0, Math.min(trajectory.duration, Number(animation.currentTime ?? 0))));
    const rate = animation.playState === 'running' && typeof animation.currentTime === 'number' && typeof animation.startTime === 'number' ? animation.playbackRate : 0;
    expect(playback.sample()).toEqual({ value: raw.value, velocity: raw.velocity * rate });
  };
  animation.currentTime = -0.000001;
  expect(animation.currentTime).toBeLessThan(0);
  check(); await animation.ready; check();
  animation.currentTime = 80; animation.updatePlaybackRate(-1);
  expect(animation.pending).toBe(true); check(); await animation.ready; check();
  animation.pause(); check(); await animation.ready; check();
  animation.currentTime = 100;
  expect(playback.sample()).toEqual({ value: springTrajectory(0, 100, 20).sample(100).value, velocity: 0 });
  animation.play(); check(); await animation.ready; check();
  animation.playbackRate = 1; animation.finish(); await playback.finished;
  expect(playback.sample()).toEqual({ value: 100, velocity: 0 });
  playback.cancel(); expect(playback.sample()).toEqual({ value: 0, velocity: 0 });
});

it.each(['before', 'after'] as const)('holds the explicit endpoint and frozen velocity %s the native interval', async boundary => {
  const element = document.body.appendChild(document.createElement('div'));
  const playback = playSpring([element], 0, 100, 20, value => ({ translate: `${value}px` }));
  void playback.finished.catch(() => {});
  const animation = playback.animations[0];
  animation.pause(); await animation.ready;
  const duration = springTrajectory(0, 100, 20).duration;
  animation.currentTime = boundary === 'before' ? -1 : duration + 1;
  expect(animation.playState).toBe('paused');
  if (boundary === 'before') expect(Number(animation.currentTime)).toBeLessThan(0);
  else expect(Number(animation.currentTime)).toBeGreaterThan(duration);
  // Independent endpoint facts, not a second invocation of the sampling algorithm.
  const endpoint = boundary === 'before' ? 0 : 100;
  expect(playback.sample()).toEqual({ value: endpoint, velocity: 0 });
  expect(getComputedStyle(element).translate).toBe(`${endpoint}px`);
  await frames(2);
  expect(playback.sample()).toEqual({ value: endpoint, velocity: 0 });
  playback.cancel();
});
