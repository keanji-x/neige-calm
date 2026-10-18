import { afterEach, expect, it, vi } from 'vitest';
import { playTrajectory } from './spring.ts';

const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
const owned: ReturnType<typeof playTrajectory<number>>[] = [];
function acquire(callback?: () => void, count = 1, duration = 2000) {
  const elements = Array.from({ length: count }, () => document.body.appendChild(document.createElement('div')));
  const playback = playTrajectory(elements, { duration, sample: time => ({ value: time / duration, velocity: 0.5 }) }, opacity => ({ opacity }), callback);
  owned.push(playback); void playback.finished.catch(() => {});
  return playback;
}
afterEach(() => { owned.forEach(playback => playback.cancel()); owned.length = 0; document.body.replaceChildren(); vi.restoreAllMocks(); });

it.each(['before', 'after'] as const)('keeps native play observing a seek %s resume', async order => {
  const notices = vi.fn(); const playback = acquire(notices); const animation = playback.animations[0];
  animation.pause(); await animation.ready; await frames(3); notices.mockClear();
  if (order === 'before') animation.currentTime = 100;
  animation.play();
  if (order === 'after') animation.currentTime = 100;
  await animation.ready; await frames(4);
  expect(notices.mock.calls.length).toBeGreaterThan(1); expect(animation.currentTime as number).toBeGreaterThan(100);
  expect(playback.sample().value).toBeCloseTo((animation.currentTime as number) / 2000, 10);
});

it('publishes paused startTime paint changes and resumes the same native clock', async () => {
  const notices = vi.fn(); const playback = acquire(notices); const animation = playback.animations[0];
  animation.pause(); await animation.ready; await frames(3); notices.mockClear();
  // WAAPI setting a resolved start time clears hold time and makes this running.
  animation.startTime = (document.timeline.currentTime as number) - 200;
  await frames(3); expect(animation.playState).toBe('running'); expect(notices.mock.calls.length).toBeGreaterThan(1);
  expect(playback.sample().value).toBeCloseTo((animation.currentTime as number) / 2000, 10);
});

it.each(['playbackRate', 'updatePlaybackRate'] as const)('keeps paused %s silent and resumes a zero rate running clock', async control => {
  const notices = vi.fn(); const request = vi.spyOn(window, 'requestAnimationFrame');
  const playback = acquire(notices); const animation = playback.animations[0];
  const rate = (value: number) => { if (control === 'playbackRate') animation.playbackRate = value; else animation.updatePlaybackRate(value); };
  animation.pause(); await animation.ready; await frames(3); notices.mockClear(); request.mockClear();
  rate(0.5); await frames(20); expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(20);
  animation.play(); await animation.ready; await frames(3);
  rate(0); await animation.ready; await frames(3); notices.mockClear(); request.mockClear();
  await frames(20); expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(20);
  rate(0.5); await animation.ready; await frames(4); expect(notices.mock.calls.length).toBeGreaterThan(1);
});

it('rearms native reverse after a pause and coalesces paired controls without changing identity', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  await Promise.all([a.ready, b.ready]); expect(a.startTime).toBe(b.startTime);
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(3); notices.mockClear();
  a.currentTime = 400; b.currentTime = 400; await frames(2); expect(notices).toHaveBeenCalledOnce();
  notices.mockClear(); b.currentTime = 500; await frames(2); expect(notices).toHaveBeenCalledOnce();
  notices.mockClear(); a.reverse(); b.reverse(); await Promise.all([a.ready, b.ready]); await frames(4);
  expect(notices.mock.calls.length).toBeGreaterThan(1); expect(a.currentTime).toBeLessThan(400);
  expect(document.getAnimations()).toEqual([a, b]);
});

it('publishes native finish exactly once and rejects stale controls after native cancel and reacquire', async () => {
  const notices = vi.fn(); const playback = acquire(notices); const animation = playback.animations[0];
  animation.pause(); await animation.ready; await frames(3); notices.mockClear();
  animation.finish(); await playback.finished; expect(notices).toHaveBeenCalledOnce();
  await frames(3); expect(notices).toHaveBeenCalledOnce();
  animation.cancel(); notices.mockClear();
  const fresh = vi.fn(); acquire(fresh);
  animation.currentTime = 40; animation.play(); await animation.ready; await frames(4);
  expect(notices).not.toHaveBeenCalled(); expect(fresh.mock.calls.length).toBeGreaterThan(1);
  animation.cancel();
});

it('keeps default playback uninstrumented and switches visual subscriptions on the existing effect', async () => {
  const request = vi.spyOn(window, 'requestAnimationFrame'); const playback = acquire(); const animation = playback.animations[0];
  expect(Object.hasOwn(animation, 'play')).toBe(false); expect(Object.hasOwn(animation, 'currentTime')).toBe(false);
  animation.pause(); await animation.ready; await frames(3); request.mockClear();
  animation.currentTime = 80; animation.playbackRate = 0.5;
  await frames(20); expect(request).toHaveBeenCalledTimes(20);
  const notices = vi.fn(); playback.setOnVisualChange(notices); await frames(3); expect(notices).toHaveBeenCalledOnce();
  const effect = animation.effect; const ready = animation.ready; const finished = animation.finished;
  notices.mockClear(); playback.setOnVisualChange(undefined); animation.currentTime = 120; animation.play(); await animation.ready;
  request.mockClear(); await frames(20); expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(20);
  playback.setOnVisualChange(notices); await frames(3); expect(notices.mock.calls.length).toBeGreaterThan(1);
  expect(animation.effect).toBe(effect); expect(animation.finished).toBe(finished); expect(ready).toBeInstanceOf(Promise);
});

it('ignores a stale pending ready after owner cancellation', async () => {
  const notices = vi.fn(); const playback = acquire(notices); const animation = playback.animations[0];
  animation.pause(); const pending = animation.ready;
  playback.cancel(); await pending.catch(() => {}); notices.mockClear();
  animation.play(); await animation.ready; await frames(4); expect(notices).not.toHaveBeenCalled();
  animation.cancel();
});

it('coalesces reentrant control from a visual callback and preserves native rejection semantics', async () => {
  const notices = vi.fn(() => { if (notices.mock.calls.length === 1) playback.animations[0].currentTime = 100; });
  const playback = acquire(notices); const animation = playback.animations[0];
  animation.pause(); await animation.ready; await frames(4);
  expect(notices).toHaveBeenCalledTimes(2); expect(animation.currentTime).toBe(100);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20); expect(request).toHaveBeenCalledTimes(20);
  notices.mockClear(); request.mockClear();
  expect(() => { animation.currentTime = Infinity; }).toThrow();
  await frames(3); expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(3);
  expect(() => { animation.currentTime = null; }).toThrow(TypeError);
  await frames(3); expect(notices).not.toHaveBeenCalled(); expect(animation.currentTime).toBe(100);
});


it('publishes a natural terminal once and observes native replay after finish', async () => {
  const times: (CSSNumberish | null)[] = [];
  const playback = acquire(() => { times.push(playback.animations[0].currentTime); }, 1, 180);
  const animation = playback.animations[0];
  await playback.finished; expect(times.at(-1)).toBe(180); expect(times.filter(time => time === 180)).toHaveLength(1);
  animation.currentTime = 40; animation.play(); await animation.ready; await frames(3);
  expect(times.at(-1) as number).toBeGreaterThan(40);
  await animation.finished; await frames(3); expect(times.at(-1)).toBe(180); expect(times.filter(time => time === 180)).toHaveLength(2);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20); expect(request).toHaveBeenCalledTimes(20);
});
it('observes same-group borrowed play after both members drain their pause', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(4); notices.mockClear();
  const effect = b.effect; const finished = b.finished; const before = b.currentTime as number;
  expect(a.play.call(b)).toBeUndefined(); await b.ready; await frames(4);
  expect(b.playState).toBe('running'); expect(a.playState).toBe('paused'); expect(b.currentTime as number).toBeGreaterThan(before);
  expect(b.effect).toBe(effect); expect(b.finished).toBe(finished); expect(b.ready).toBe(Object.getOwnPropertyDescriptor(Animation.prototype, 'ready')!.get!.call(b));
  expect(notices.mock.calls.length).toBeGreaterThan(1);
});

it('observes same-group borrowed time setter once after both members drain their pause', async () => {
  const notices = vi.fn(); const request = vi.spyOn(window, 'requestAnimationFrame'); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(4); notices.mockClear();
  const setter = Reflect.get(Object.getOwnPropertyDescriptor(a, 'currentTime')!, 'set') as (this: Animation, value: number) => void;
  expect(setter.call(b, 300)).toBeUndefined(); await frames(3);
  expect(b.currentTime).toBe(300); expect(a.currentTime).not.toBe(300); expect(notices).toHaveBeenCalledOnce();
  request.mockClear(); await frames(20); expect(notices).toHaveBeenCalledOnce(); expect(request).toHaveBeenCalledTimes(20);
});

it.each(['reverse', 'finish', 'updatePlaybackRate', 'startTime', 'playbackRate'] as const)('observes same-group borrowed %s on the nonprimary clock', async control => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]);
  a.currentTime = 400; b.currentTime = 400; await frames(3);
  if (control === 'playbackRate' || control === 'updatePlaybackRate') {
    b.playbackRate = -0; b.play(); await b.ready; await frames(3);
  }
  notices.mockClear();
  const effect = b.effect; const finished = b.finished;
  if (control === 'reverse') expect(a.reverse.call(b)).toBeUndefined();
  else if (control === 'finish') expect(a.finish.call(b)).toBeUndefined();
  else if (control === 'updatePlaybackRate') expect(a.updatePlaybackRate.call(b, 0.5)).toBeUndefined();
  else Object.getOwnPropertyDescriptor(a, control)!.set!.call(b, control === 'startTime' ? (document.timeline.currentTime as number) - 400 : 0.5);
  const ready = b.ready; await ready; await frames(3);
  expect(a.currentTime).toBe(400); expect(b.effect).toBe(effect); expect(b.finished).toBe(finished); expect(b.ready).toBe(ready);
  if (control === 'finish') { expect(b.currentTime).toBe(2000); expect(notices).toHaveBeenCalledOnce(); }
  else { expect(notices.mock.calls.length).toBeGreaterThan(1); expect(b.currentTime).not.toBe(400); }
});

it('borrows pause and paused negative-zero rate controls without idle work', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); await a.ready; await frames(3);
  expect(a.pause.call(b)).toBeUndefined(); await b.ready; await frames(3);
  expect(b.playState).toBe('paused'); notices.mockClear();
  a.updatePlaybackRate.call(b, -0); Object.getOwnPropertyDescriptor(a, 'playbackRate')!.set!.call(b, -0);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20);
  expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(20);
  a.play.call(b); const ready = b.ready; await ready; await frames(3); notices.mockClear(); request.mockClear();
  await frames(20); expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(20);
  a.updatePlaybackRate.call(b, 0.5); await b.ready; await frames(3); expect(notices.mock.calls.length).toBeGreaterThan(1);
});

it('revokes the entire group on borrowed member cancel and never restores old authority', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(3); notices.mockClear();
  const oldPlay = Reflect.get(a, 'play'); const oldTime = Reflect.get(Object.getOwnPropertyDescriptor(a, 'currentTime')!, 'set') as (this: Animation, value: number) => void;
  const request = vi.spyOn(window, 'requestAnimationFrame');
  expect(a.cancel.call(b)).toBeUndefined();
  // These controls precede the rejected finished reaction: revocation must be synchronous.
  oldTime.call(a, 300); oldPlay.call(a); playback.setOnVisualChange(undefined); playback.setOnVisualChange(notices);
  expect(request).not.toHaveBeenCalled(); await a.ready; await frames(4);
  expect(notices).not.toHaveBeenCalled(); expect(request).toHaveBeenCalledTimes(4);
  const fresh = vi.fn(); acquire(fresh, 2); await frames(3); expect(fresh.mock.calls.length).toBeGreaterThan(1);
  notices.mockClear(); oldTime.call(b, 100); oldPlay.call(b); await b.ready; b.finish(); await frames(3);
  expect(notices).not.toHaveBeenCalled();
});

it('coalesces reentrant borrowed seeks and callback disable and enable on the same group', async () => {
  const playback = acquire(undefined, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]);
  const notices = vi.fn(() => { if (notices.mock.calls.length === 1) Object.getOwnPropertyDescriptor(a, 'currentTime')!.set!.call(b, 300); });
  playback.setOnVisualChange(notices); await frames(4); expect(notices).toHaveBeenCalledTimes(2); expect(b.currentTime).toBe(300);
  playback.setOnVisualChange(undefined); notices.mockClear();
  Object.getOwnPropertyDescriptor(a, 'currentTime')!.set!.call(b, 400); a.play.call(b); await b.ready;
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(notices).not.toHaveBeenCalled();
  playback.setOnVisualChange(notices); await frames(3); expect(notices.mock.calls.length).toBeGreaterThan(1);
});

it.each(['play', 'pause', 'reverse', 'finish', 'cancel', 'updatePlaybackRate'] as const)('preserves native %s receiver returns and errors without discovering a foreign owner', async control => {
  const notices = vi.fn(); const foreignNotices = vi.fn(); const playback = acquire(notices, 2); const foreign = acquire(foreignNotices, 2); const [a, b] = playback.animations; const c = foreign.animations[0];
  for (const animation of [...playback.animations, ...foreign.animations]) animation.pause(); await Promise.all([...playback.animations, ...foreign.animations].map(animation => animation.ready));
  c.currentTime = 400; await frames(3); notices.mockClear(); foreignNotices.mockClear();
  const native = Reflect.get(Animation.prototype, control); const own = Reflect.get(a, control);
  const failure = (method: typeof native) => { try { Reflect.apply(method, {}, [0.5]); } catch (error) { return error; } throw new Error('Missing native brand error'); };
  const expected = failure(native); const actual = failure(own);
  expect(actual).toBeInstanceOf(TypeError); expect((actual as Error).message).toBe((expected as Error).message);
  const effect = c.effect;
  expect(Reflect.apply(own, c, [0.5])).toBeUndefined(); expect(c.effect).toBe(effect);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3);
  expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled(); expect(foreignNotices).not.toHaveBeenCalled();
  // In particular foreign cancel must not revoke this group.
  Object.getOwnPropertyDescriptor(a, 'currentTime')!.set!.call(b, 300); await frames(3); expect(notices).toHaveBeenCalledOnce();
});

it.each(['currentTime', 'startTime', 'playbackRate'] as const)('preserves borrowed native %s getter setter brands and thrown identity', async control => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const foreign = acquire(); const [a, b] = playback.animations; const c = foreign.animations[0];
  for (const animation of [a, b, c]) animation.pause(); await Promise.all([a.ready, b.ready, c.ready]); await frames(3); notices.mockClear();
  const own = Object.getOwnPropertyDescriptor(a, control) as { get: (this: Animation) => unknown; set: (this: Animation, value: unknown) => void };
  const native = Object.getOwnPropertyDescriptor(Animation.prototype, control) as typeof own;
  expect(own.get).toBe(native.get); expect(own.get.call(b)).toBe(native.get.call(b));
  for (const operation of ['get', 'set'] as const) {
    const error = (descriptor: typeof own) => { try { Reflect.apply(descriptor[operation], {}, [20]); } catch (error) { return error; } throw new Error('Missing brand error'); };
    expect(error(own)).toBeInstanceOf(TypeError); expect((error(own) as Error).message).toBe((error(native) as Error).message);
  }
  const sentinel = new Error('numeric conversion'); const value = { valueOf() { throw sentinel; } };
  expect(() => own.set.call(b, value)).toThrow(sentinel);
  try { own.set.call(b, value); } catch (error) { expect(error).toBe(sentinel); }
  expect(own.set.call(c, control === 'startTime' ? (document.timeline.currentTime as number) - 400 : 0.5)).toBeUndefined();
  expect(own.get.call(c)).toBe(native.get.call(c));
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3); expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
});

it('preserves native method conversion exception identity before group notification', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(3); notices.mockClear();
  const sentinel = new Error('rate conversion'); const value = { valueOf() { throw sentinel; } };
  try { Reflect.apply(Reflect.get(a, 'updatePlaybackRate'), b, [value]); throw new Error('Missing conversion error'); } catch (error) { expect(error).toBe(sentinel); }
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3); expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
});

it('preflights every group member before installing controls or retaining a callback', async () => {
  const notices = vi.fn(); const playback = acquire(undefined, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); Object.preventExtensions(b);
  const definitions = vi.spyOn(Object, 'defineProperty');
  expect(() => playback.setOnVisualChange(notices)).toThrow(TypeError);
  expect(definitions.mock.calls.filter(([target]) => target === a || target === b)).toEqual([]); definitions.mockRestore();
  expect(Object.hasOwn(a, 'play')).toBe(false); expect(Object.hasOwn(a, 'currentTime')).toBe(false);
  expect(() => playback.setOnVisualChange(notices)).toThrow(TypeError);
  a.play(); await a.ready; const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3);
  expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
});

it('rejects unavailable public descriptors before any group installation and permits a clean retry', async () => {
  const notices = vi.fn(); const playback = acquire(undefined, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]);
  const descriptor = Object.getOwnPropertyDescriptor(Animation.prototype, 'startTime')!;
  try {
    Object.defineProperty(Animation.prototype, 'startTime', { ...descriptor, set: undefined });
    expect(() => playback.setOnVisualChange(notices)).toThrow(TypeError);
    expect(Object.hasOwn(a, 'play')).toBe(false); expect(Object.hasOwn(b, 'play')).toBe(false);
  } finally { Object.defineProperty(Animation.prototype, 'startTime', descriptor); }
  playback.setOnVisualChange(notices); await frames(3); expect(notices).toHaveBeenCalledOnce();
});

it('rolls back all installed controls on definition failure and retries with no half-installed callback', async () => {
  const notices = vi.fn(); const playback = acquire(undefined, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]);
  const define = Object.defineProperty; const sentinel = new Error('definition refused');
  const spy = vi.spyOn(Object, 'defineProperty').mockImplementation((target, name, descriptor) => {
    if (target === b && name === 'currentTime') throw sentinel;
    return define(target, name, descriptor);
  });
  try { playback.setOnVisualChange(notices); throw new Error('Missing installation error'); } catch (error) { expect(error).toBe(sentinel); } finally { spy.mockRestore(); }
  expect(Object.getOwnPropertyNames(a)).toEqual([]); expect(Object.getOwnPropertyNames(b)).toEqual([]);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3); expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
  playback.setOnVisualChange(notices); await frames(3); expect(notices).toHaveBeenCalledOnce();
  expect(Object.getOwnPropertyNames(a)).toHaveLength(9); expect(Object.getOwnPropertyNames(b)).toHaveLength(9);
});

it('retains native finish errors for a borrowed zero-rate receiver without notification', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); b.playbackRate = -0; await frames(3); notices.mockClear();
  const failure = (method: Animation['finish']) => { try { method.call(b); } catch (error) { return error as DOMException; } throw new Error('Missing finish error'); };
  const expected = failure(Reflect.get(Animation.prototype, 'finish')); const actual = failure(Reflect.get(a, 'finish'));
  expect(actual.constructor).toBe(expected.constructor); expect(actual.name).toBe('InvalidStateError'); expect(actual.message).toBe(expected.message);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3); expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
});

it('replays a finished nonprimary group member through the borrowed controls then idles', async () => {
  const notices = vi.fn(); const playback = acquire(notices, 2, 180); const [a, b] = playback.animations;
  a.pause(); b.pause(); await Promise.all([a.ready, b.ready]); await frames(3); notices.mockClear();
  a.finish.call(b); await b.finished; await frames(3); expect(notices).toHaveBeenCalledOnce();
  const finished = b.finished; notices.mockClear();
  Object.getOwnPropertyDescriptor(a, 'currentTime')!.set!.call(b, 40); a.play.call(b); await b.ready; await frames(3);
  expect(b.finished).not.toBe(finished); expect(b.finished).toBe(Object.getOwnPropertyDescriptor(Animation.prototype, 'finished')!.get!.call(b));
  expect(notices.mock.calls.length).toBeGreaterThan(1); await b.finished; await frames(3);
  expect(b.currentTime).toBe(180); const count = notices.mock.calls.length;
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(notices).toHaveBeenCalledTimes(count);
});

it('cancels all newly acquired native effects when initial control installation fails', async () => {
  const notices = vi.fn(); const animations = new Set<Animation>(); const define = Object.defineProperty; const sentinel = new Error('initial definition refused');
  const spy = vi.spyOn(Object, 'defineProperty').mockImplementation((target, name, descriptor) => {
    if (target instanceof Animation) {
      animations.add(target);
      if (animations.size === 2 && name === 'currentTime') throw sentinel;
    }
    return define(target, name, descriptor);
  });
  try { acquire(notices, 2); throw new Error('Missing initial installation error'); } catch (error) { expect(error).toBe(sentinel); } finally { spy.mockRestore(); }
  expect(animations.size).toBe(2); expect([...animations].map(animation => animation.playState)).toEqual(['idle', 'idle']);
  expect(document.getAnimations()).toEqual([]);
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(3); expect(request).toHaveBeenCalledTimes(3); expect(notices).not.toHaveBeenCalled();
});
