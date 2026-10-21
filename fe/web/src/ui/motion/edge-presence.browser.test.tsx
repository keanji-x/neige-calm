import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands } from 'vitest/browser';
import { useEdgePresence, type EdgePresenceInput } from './edge-presence.ts';
import { readSurfaceFrame } from './geometry.ts';
import { playTrajectory, type Playback } from './spring.ts';
vi.mock('./spring.ts', { spy: true });

function Probe(props: EdgePresenceInput) { useEdgePresence(props); return null; }
const frames: HTMLElement[] = [];
function fixture(edge: EdgePresenceInput['edge'] = 'inline-end', direction = 'ltr') {
  const frame = document.createElement('div');
  frame.style.cssText = 'position:absolute;left:40px;top:60px;width:240px;height:160px;transform-origin:0 0;transform:scale(1.25,0.75)';
  const node = frame.appendChild(document.createElement('div'));
  node.style.cssText = 'width:100%;height:100%;transform:none'; node.dir = direction;
  document.body.append(frame); frames.push(frame);
  const props: EdgePresenceInput = { frame: { ref: { current: frame } }, part: { element: { current: node }, readFrame: () => readSurfaceFrame(frame) },
    edge, open: true, present: true, enabled: true, onExited: vi.fn() };
  return { frame, node, props };
}
function latest(): Playback<number> { return vi.mocked(playTrajectory).mock.results.at(-1)!.value as Playback<number>; }
function matrix(node: HTMLElement) { return new DOMMatrix(getComputedStyle(node).transform); }
afterEach(async () => { cleanup(); frames.splice(0).forEach(frame => frame.remove()); vi.restoreAllMocks(); vi.clearAllMocks(); await commands.emulateReducedMotion(false); });

it.each([
  ['inline-end', 'ltr', 300], ['inline-end', 'rtl', -300],
  ['inline-start', 'ltr', -300], ['inline-start', 'rtl', 300],
  ['bottom', 'ltr', 120], ['bottom', 'rtl', 120],
] as const)('uses %s in %s as physical viewport pixels with unit text scale', (edge, direction, distance) => {
  const f = fixture(edge, direction); render(<Probe {...f.props} />);
  const animation = f.node.getAnimations()[0]; animation.pause(); animation.currentTime = 0;
  const base = f.frame.getBoundingClientRect(); const painted = f.node.getBoundingClientRect();
  expect(edge === 'bottom' ? painted.top - base.top : painted.left - base.left).toBeCloseTo(distance, 5);
  const m = matrix(f.node);
  expect(m.a).toBe(1); expect(m.d).toBe(1); expect(m.b).toBe(0); expect(m.c).toBe(0);
  expect(getComputedStyle(f.node).opacity).toBe('1');
  expect(f.frame.getAnimations()).toHaveLength(0);
});

it.each(['inline-end', 'bottom'] as const)('retains %s contribution and signed wall-clock velocity on frame resize and reversal', async edge => {
  const f = fixture(edge); const view = render(<Probe {...f.props} />);
  const old = latest(); const native = old.animations[0]; await native.ready;
  native.playbackRate = -0.5; native.currentTime = 80;
  native.startTime = Number(document.timeline.currentTime) + 160;
  const before = old.sample(); expect(Math.abs(before.velocity)).toBeGreaterThan(1);
  // A committed frame change occurs without advancing the document clock.
  f.frame.style.width = '350px'; f.frame.style.height = '270px';
  view.rerender(<Probe {...f.props} />);
  const after = latest().sample();
  expect(after.value).toBeCloseTo(before.value, 6);
  // Newly acquired native clocks can be pending: the trajectory itself pins
  // the initial derivative, independently of native ready settling.
  const initial = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(initial.velocity).toBeCloseTo(before.velocity, 6);
  expect(native.playState).toBe('idle');
  const moving = latest(); await moving.animations[0].ready;
  moving.animations[0].currentTime = 55;
  moving.animations[0].startTime = Number(document.timeline.currentTime) - 55;
  const reverseFrom = moving.sample();
  view.rerender(<Probe {...f.props} open={false} />);
  const reverse = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(reverse.value).toBeCloseTo(reverseFrom.value, 6);
  expect(reverse.velocity).toBeCloseTo(reverseFrom.velocity, 6);
});

it('allows ordinary layout and scroll displacement without locking screen coordinates', () => {
  const f = fixture(); const view = render(<Probe {...f.props} />);
  const animation = f.node.getAnimations()[0]; animation.pause(); animation.currentTime = 70;
  const before = f.node.getBoundingClientRect();
  f.frame.style.left = '90px'; f.frame.style.top = '100px';
  view.rerender(<Probe {...f.props} />);
  expect(f.node.getAnimations()[0]).toBe(animation);
  const after = f.node.getBoundingClientRect();
  expect(after.left - before.left).toBeCloseTo(50, 4); expect(after.top - before.top).toBeCloseTo(40, 4);
  const scroller = document.createElement('div');
  scroller.style.cssText = 'position:absolute;left:0;top:0;width:700px;height:200px;overflow:auto';
  const content = scroller.appendChild(document.createElement('div'));
  content.style.cssText = 'height:1000px;position:relative'; content.append(f.frame);
  document.body.append(scroller); frames.push(scroller);
  const top = f.node.getBoundingClientRect().top;
  scroller.scrollTop = 60;
  view.rerender(<Probe {...f.props} />);
  expect(f.node.getBoundingClientRect().top - top).toBeCloseTo(-60, 4);
  expect(f.node.getAnimations()[0]).toBe(animation);
});

it('transfers node identity and suspends missing frames without granting an old completion', async () => {
  const f = fixture(); const view = render(<Probe {...f.props} />);
  const old = latest(); await old.animations[0].ready;
  old.animations[0].currentTime = 70; old.animations[0].startTime = Number(document.timeline.currentTime) - 70;
  const before = old.sample();
  const replacement = f.node.cloneNode() as HTMLElement; f.node.replaceWith(replacement);
  const props = { ...f.props, part: { ...f.props.part, element: { current: replacement } } };
  view.rerender(<Probe {...props} />);
  expect(f.node.getAnimations()).toHaveLength(0);
  const transferred = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(transferred.value).toBeCloseTo(before.value, 6); expect(transferred.velocity).toBeCloseTo(before.velocity, 6);
  old.animations[0].finish(); await Promise.resolve(); expect(props.onExited).not.toHaveBeenCalled();
  const active = latest(); active.animations[0].pause(); active.animations[0].currentTime = 55;
  const paused = active.sample();
  view.rerender(<Probe {...props} part={{ ...props.part, readFrame: () => null }} open={false} />);
  expect(replacement.getAnimations()).toHaveLength(0); expect(props.onExited).not.toHaveBeenCalled();
  view.rerender(<Probe {...props} open={false} />);
  const resumed = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(resumed.value).toBeCloseTo(paused.value, 6); expect(resumed.velocity).toBeCloseTo(paused.velocity, 6);
  latest().animations[0].finish(); await expect.poll(() => vi.mocked(props.onExited).mock.calls.length).toBe(1);
});

it('reduced motion and disable settle without effects and present release rejects stale finish', async () => {
  const f = fixture(); const view = render(<Probe {...f.props} />); const old = latest().animations[0];
  await commands.emulateReducedMotion(true);
  await expect.poll(() => f.node.getAnimations().length).toBe(0);
  view.rerender(<Probe {...f.props} open={false} />);
  expect(f.props.onExited).toHaveBeenCalledTimes(1);
  view.rerender(<Probe {...f.props} enabled={false} />);
  expect(matrix(f.node).m41).toBe(0);
  view.rerender(<Probe {...f.props} present={false} />);
  old.finish(); await Promise.resolve(); expect(f.props.onExited).toHaveBeenCalledTimes(1);
});

it('natural opening and closing finish once and unmount cancels remaining paint', async () => {
  const f = fixture(); const view = render(<Probe {...f.props} />);
  const opening = latest().animations[0];
  await opening.finished;
  await expect.poll(() => f.node.getAnimations().length).toBe(0);
  expect(matrix(f.node).m41).toBe(0);
  view.rerender(<Probe {...f.props} open={false} />);
  await latest().finished;
  await expect.poll(() => vi.mocked(f.props.onExited).mock.calls.length).toBe(1);
  view.rerender(<Probe {...f.props} />);
  const reopened = latest().animations[0];
  view.unmount(); expect(f.node.getAnimations()).toHaveLength(0);
  reopened.finish(); await Promise.resolve(); expect(f.props.onExited).toHaveBeenCalledTimes(1);
});


it('naturally resizes during opening without losing physical contribution or velocity', async () => {
  const f = fixture(); render(<Probe {...f.props} />);
  const old = latest(); const native = old.animations[0]; await native.ready;
  await new Promise(requestAnimationFrame);
  let before: ReturnType<typeof old.sample> | undefined;
  vi.spyOn(native, 'cancel').mockImplementation(function (this: Animation) {
    before = old.sample(); Animation.prototype.cancel.call(this);
  });
  f.frame.style.width = '400px';
  await expect.poll(() => f.node.getAnimations().some(animation => animation !== native)).toBe(true);
  expect(before).toBeDefined();
  const next = vi.mocked(playTrajectory).mock.calls.at(-1)![1].sample(0);
  expect(next.value).toBeCloseTo(before!.value, 6);
  expect(next.velocity).toBeCloseTo(before!.velocity, 6);
  expect(Math.abs(before!.velocity)).toBeGreaterThan(1);
  await latest().finished;
  expect(matrix(f.node).m41).toBe(0);
});

it('completion uses latest declaration when ref object changes on same paint element', async () => {
  const f = fixture(); const oldRef = f.props.part.element;
  const view = render(<Probe {...f.props} />);
  const opening = latest().animations[0]; await opening.ready;
  opening.currentTime = 80; opening.startTime = Number(document.timeline.currentTime) - 80;
  view.rerender(<Probe {...f.props} open={false} />);
  const closing = latest().animations[0];
  oldRef.current = null;
  const onExited = vi.fn();
  const part = { element: { current: f.node }, readFrame: () => readSurfaceFrame(f.frame) };
  view.rerender(<Probe {...f.props} part={part} open={false} onExited={onExited} />);
  expect(f.node.getAnimations()[0]).toBe(closing);
  closing.finish(); await closing.finished;
  await Promise.resolve(); await Promise.resolve();
  expect(onExited).toHaveBeenCalledTimes(1);
  expect(f.props.onExited).not.toHaveBeenCalled();
  view.rerender(<Probe {...f.props} part={part} open={false} onExited={onExited} />);
  expect(onExited).toHaveBeenCalledTimes(1);
});

it('independent unanimated frame resize invalidates active edge paint without React render', async () => {
  const f = fixture(); f.node.style.width = '120px'; f.node.style.height = '100px';
  render(<Probe {...f.props} />);
  const old = latest().animations[0]; await old.ready; old.playbackRate = 0;
  // Drain initial observer delivery before changing only the layout frame.
  for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
  const paintedWidth = f.node.offsetWidth;
  f.frame.style.width = '400px';
  for (let i = 0; i < 5; i++) await new Promise(requestAnimationFrame);
  expect(f.node.offsetWidth).toBe(paintedWidth);
  expect(f.node.getAnimations()[0]).not.toBe(old);
  expect(old.playState).toBe('idle');
});

it('refreshes frame and paint observations on ref replacement and releases them with presence', async () => {
  const observe = vi.spyOn(ResizeObserver.prototype, 'observe');
  const disconnect = vi.spyOn(ResizeObserver.prototype, 'disconnect');
  const f = fixture(); const other = fixture();
  const view = render(<Probe {...f.props} />);
  const native = latest().animations[0]; await native.ready; native.playbackRate = 0;
  for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
  expect(observe.mock.calls.map(call => call[0])).toEqual([f.node, f.frame]);
  const frameRef = { current: f.frame as HTMLElement | null };
  const props = { ...f.props, frame: { ref: frameRef },
    part: { element: { current: f.node }, readFrame: () => readSurfaceFrame(frameRef.current) } };
  view.rerender(<Probe {...props} />);
  expect(disconnect).toHaveBeenCalledTimes(1);
  expect(f.node.getAnimations()[0]).toBe(native);
  props.frame.ref.current = other.frame;
  view.rerender(<Probe {...props} />);
  expect(disconnect).toHaveBeenCalledTimes(2);
  expect(observe.mock.calls.slice(-2).map(call => call[0])).toEqual([f.node, other.frame]);
  for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
  const calls = vi.mocked(playTrajectory).mock.calls.length;
  f.frame.style.width = '350px';
  for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
  expect(vi.mocked(playTrajectory)).toHaveBeenCalledTimes(calls);
  other.frame.style.width = '400px';
  await expect.poll(() => f.node.getAnimations()[0] !== native).toBe(true);
  props.frame.ref.current = null;
  view.rerender(<Probe {...props} open={false} />);
  expect(f.node.getAnimations()).toHaveLength(0);
  expect(props.onExited).not.toHaveBeenCalled();
  props.frame.ref.current = other.frame;
  view.rerender(<Probe {...props} open={false} />);
  const closing = latest().animations[0];
  const beforeRelease = disconnect.mock.calls.length;
  view.rerender(<Probe {...props} open={false} present={false} />);
  expect(disconnect).toHaveBeenCalledTimes(beforeRelease + 1);
  expect(f.node.getAnimations()).toHaveLength(0);
  const releasedCalls = vi.mocked(playTrajectory).mock.calls.length;
  other.frame.style.width = '450px'; f.node.style.width = '100px';
  for (let i = 0; i < 4; i++) await new Promise(requestAnimationFrame);
  expect(vi.mocked(playTrajectory)).toHaveBeenCalledTimes(releasedCalls);
  closing.finish(); await Promise.resolve();
  expect(props.onExited).not.toHaveBeenCalled();
});
