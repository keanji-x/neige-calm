import { afterEach, expect, it, vi } from 'vitest';
import { playSpring } from './spring.ts';

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
