import { afterEach, expect, it, vi } from 'vitest';
import { playPointSpring, playSpring } from './spring.ts';

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
  outward.animations[0].pause(); outward.animations[0].currentTime = 80;
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
