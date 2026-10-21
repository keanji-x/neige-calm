import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands } from 'vitest/browser';
import { useSurfacePresence, type SurfacePresenceInput } from './surface-presence.ts';
import { readSurfaceFrame } from './geometry.ts';
import { playTrajectory } from './spring.ts';
vi.mock('./spring.ts', { spy: true });

function Probe(props: SurfacePresenceInput) { useSurfacePresence(props); return null; }
function part(x: number, sx: number, sy: number) {
  const frame = document.createElement('div');
  frame.style.cssText = `position:fixed;left:${x}px;top:100.25px;width:120.5px;height:160.25px;transform-origin:0 0;transform:scale(${sx},${sy});zoom:1.125`;
  const node = frame.appendChild(document.createElement('div'));
  node.style.cssText = 'width:100%;height:100%'; document.body.append(frame);
  return { frame, node, element: { current: node as HTMLElement | null }, readFrame: () => readSurfaceFrame(frame) };
}
function fixture(direction = 'ltr') {
  const primary = part(250.75, 1.25, 0.75); const peer = part(410.375, 0.5, 1.5);
  primary.node.dir = direction;
  const props: SurfacePresenceInput = { parts: { kind: 'paired', primary, peer },
    geometry: { kind: 'edge-reveal', origin: { kind: 'paired-edge', edge: 'inline-end' } },
    open: true, present: true, enabled: true, onExited: vi.fn() };
  return { primary, peer, props };
}
const created: HTMLElement[] = [];
function setup(direction?: string) { const f = fixture(direction); created.push(f.primary.frame, f.peer.frame); return f; }
function pause(node: HTMLElement, time = 0) {
  const a = node.getAnimations()[0]; expect(a).toBeDefined(); a.pause(); a.currentTime = time; return a;
}
function matrix(node: HTMLElement) { return new DOMMatrix(getComputedStyle(node).transform); }
type Sample = { value: { progress: number; offsets: { x: number; y: number }[] }; velocity: { progress: number; offsets: { x: number; y: number }[] } };
function trajectory() { return vi.mocked(playTrajectory).mock.calls.at(-1)![1] as { sample: (time: number) => Sample }; }
afterEach(async () => { cleanup(); created.splice(0).forEach(node => node.remove()); vi.clearAllMocks(); await commands.emulateReducedMotion(false); });

it.each(['ltr', 'rtl'])('uses the primary %s logical edge camera with unit scale in fractional independent frames and zoom', direction => {
  const { primary, peer, props } = setup(direction); render(<Probe {...props} />);
  const a = pause(primary.node); const b = pause(peer.node);
  const primaryBox = primary.frame.getBoundingClientRect();
  // Independent camera oracle at p=0: D=(1-800/808)*(O-C)+(0,8*.07).
  const dx = (1 - 800 / 808) * primaryBox.width / 2 * (direction === 'rtl' ? -1 : 1);
  for (const item of [primary, peer]) {
    const box = item.frame.getBoundingClientRect(); const painted = item.node.getBoundingClientRect(); const m = matrix(item.node);
    expect(m.a).toBe(1); expect(m.d).toBe(1); expect(m.b).toBe(0); expect(m.c).toBe(0);
    expect(painted.left - box.left).toBeCloseTo(dx, 3); expect(painted.top - box.top).toBeCloseTo(0.56, 3);
    expect(painted.width).toBeCloseTo(box.width, 3); expect(painted.height).toBeCloseTo(box.height, 3);
    expect(item.frame.getAnimations()).toHaveLength(0); expect(item.node.getAnimations()).toHaveLength(1);
  }
  expect(a.startTime).toBe(b.startTime);
});

it.each(['primary', 'peer'] as const)('preserves each local motion contribution and analytical velocity when %s alone resizes and changes scale', changed => {
  const f = setup(); const view = render(<Probe {...f.props} />);
  for (const item of [f.primary, f.peer]) { const a = item.node.getAnimations()[0]; a.startTime = document.timeline.currentTime; a.currentTime = 80; }
  const old = trajectory(); const before = [matrix(f.primary.node), matrix(f.peer.node)];
  const boxes = [f.primary.frame.getBoundingClientRect(), f.peer.frame.getBoundingClientRect()];
  const oldFrames = [f.primary.readFrame()!, f.peer.readFrame()!];
  const from = old.sample(80); const p = from.value.progress;
  // Analytical derivative of 1-800/(800+8*(1-p)). No production map/helper is used.
  const speed = -6400 / (808 - 8 * p) ** 2 * from.velocity.progress;
  const velocities = oldFrames.map((frame, i) => ({ x: (speed * boxes[0].width / 2 + from.velocity.offsets[i].x) / frame.viewportFromLocal.scaleX,
    y: (-0.56 * from.velocity.progress + from.velocity.offsets[i].y) / frame.viewportFromLocal.scaleY }));
  f[changed].frame.style.width = '180.75px'; f[changed].frame.style.transform = 'scale(1.6, .85)';
  view.rerender(<Probe {...f.props} />);
  pause(f.primary.node); pause(f.peer.node);
  const next = trajectory(); const dt = 1;
  const nextSample = next.sample(0);
  const nextSpeed = -6400 / (808 - 8 * nextSample.value.progress) ** 2 * nextSample.velocity.progress;
  const nextPrimaryBox = f.primary.frame.getBoundingClientRect();
  for (const [i, item] of [f.primary, f.peer].entries()) {
    expect(matrix(item.node).e).toBeCloseTo(before[i].e, 4); expect(matrix(item.node).f).toBeCloseTo(before[i].f, 4);
    const frame = item.readFrame()!;
    expect((nextSpeed * nextPrimaryBox.width / 2 + nextSample.velocity.offsets[i].x) / frame.viewportFromLocal.scaleX).toBeCloseTo(velocities[i].x, 8);
    expect((-0.56 * nextSample.velocity.progress + nextSample.velocity.offsets[i].y) / frame.viewportFromLocal.scaleY).toBeCloseTo(velocities[i].y, 8);
    // Observe actual emitted keyframes just after zero, rather than calling the producer's rebase helper.
    const animation = item.node.getAnimations()[0]; animation.currentTime = dt;
    expect((matrix(item.node).e - before[i].e) / dt * 1000).toBeCloseTo(velocities[i].x, 0);
    expect((matrix(item.node).f - before[i].f) / dt * 1000).toBeCloseTo(velocities[i].y, 0);
    expect(next.sample(0).velocity.progress).toBeCloseTo(from.velocity.progress, 10);
  }
  view.unmount(); expect(f.primary.node.getAnimations()).toHaveLength(0); expect(f.peer.node.getAnimations()).toHaveLength(0);
});

it('rebases a map change on the same nodes instead of swallowing it as equal geometry', () => {
  const { primary, peer, props } = setup(); const view = render(<Probe {...props} geometry={{ kind: 'lift', origin: { kind: 'paired-edge', edge: 'inline-end' } }} />);
  const old = pause(primary.node, 80); pause(peer.node, 80);
  const before = [matrix(primary.node), matrix(peer.node)];
  view.rerender(<Probe {...props} />);
  for (const [i, item] of [primary, peer].entries()) {
    pause(item.node); expect(matrix(item.node).a).toBe(1);
    expect(matrix(item.node).e).toBeCloseTo(before[i].e, 4); expect(matrix(item.node).f).toBeCloseTo(before[i].f, 4);
  }
  expect(old.playState).toBe('idle');
});

it('keeps native pause and reversal identity, suspends null frames, transfers replacement slots and ignores stale completion', async () => {
  const { primary, peer, props } = setup(); let ready = true;
  const parts = { kind: 'paired' as const, primary: { ...primary, readFrame: () => ready ? primary.readFrame() : null }, peer };
  const view = render(<Probe {...props} parts={parts} />);
  const old = pause(primary.node, 80); pause(peer.node, 80); const before = matrix(primary.node);
  view.rerender(<Probe {...props} parts={parts} open={false} />);
  const close = pause(primary.node); pause(peer.node); expect(matrix(primary.node).e).toBeCloseTo(before.e, 4);
  view.rerender(<Probe {...props} parts={parts} />); pause(primary.node); pause(peer.node);
  expect(old.playState).toBe('idle'); expect(close.playState).toBe('idle'); expect(props.onExited).not.toHaveBeenCalled();
  ready = false; view.rerender(<Probe {...props} parts={parts} />);
  expect(primary.node.getAnimations()).toHaveLength(0); expect(peer.node.getAnimations()).toHaveLength(0);
  const replacement = primary.frame.appendChild(primary.node.cloneNode() as HTMLElement); primary.element.current = replacement;
  ready = true; view.rerender(<Probe {...props} parts={parts} open={false} />);
  pause(replacement); pause(peer.node); expect(primary.node.getAnimations()).toHaveLength(0);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => vi.mocked(props.onExited).mock.calls.length).toBe(1);
  expect(replacement.getAnimations()).toHaveLength(0); expect(peer.node.getAnimations()).toHaveLength(0);
});

it('cleans the whole edge group on native acquisition failure and rejects unsupported frames', () => {
  const { primary, peer, props } = setup();
  const failure = vi.spyOn(peer.node, 'animate').mockImplementation(() => { throw new Error('edge acquisition failed'); });
  expect(() => render(<Probe {...props} />)).toThrow('edge acquisition failed');
  expect(primary.node.getAnimations()).toHaveLength(0); expect(peer.node.getAnimations()).toHaveLength(0);
  failure.mockRestore();
  primary.frame.style.rotate = '5deg';
  expect(() => render(<Probe {...props} />)).toThrow('Unsupported surface transform');
  expect(primary.node.getAnimations()).toHaveLength(0);
});
