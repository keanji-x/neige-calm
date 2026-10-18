import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands } from 'vitest/browser';
import { StrictMode, useRef, type RefObject } from 'react';
import { useSurfacePresence, type SurfacePresenceInput } from './surface-presence.ts';
import { readSurfaceFrame, type Point, type SurfaceFrame } from './geometry.ts';
import { liftProjection } from './depth.ts';
import { playTrajectory } from './spring.ts';
vi.mock('./spring.ts', { spy: true });
type Sample = { value: { progress: number; offsets: readonly Point[] }; velocity: { progress: number; offsets: readonly Point[] } };
function activeTrajectory() { return vi.mocked(playTrajectory).mock.calls.at(-1)![1]; }
function localVelocity(sample: Sample, frame: SurfaceFrame, origin: Point, index: number) {
  const projection = liftProjection(sample.value.progress); const ds = projection.scaleDerivative * sample.velocity.progress;
  return { x: (-ds * (origin.x - frame.viewportFromLocal.x) + sample.velocity.offsets[index].x) / frame.viewportFromLocal.scaleX,
    y: (-ds * (origin.y - frame.viewportFromLocal.y) + projection.sinkDerivative * sample.velocity.progress + sample.velocity.offsets[index].y) / frame.viewportFromLocal.scaleY };
}

function Probe(props: SurfacePresenceInput) { useSurfacePresence(props); return null; }
function part(x: number, y: number, scaleX = 1, scaleY = 1) {
  const layout = document.createElement('div');
  layout.dataset.surfaceFixture = '';
  layout.style.cssText = `position:fixed;left:${x}px;top:${y}px;width:120.5px;height:60.25px;transform-origin:0 0;transform:scale(${scaleX},${scaleY})`;
  const node = layout.appendChild(document.createElement('div'));
  node.style.cssText = 'width:100%;height:100%;background:rgb(30,60,90)';
  document.body.append(layout);
  return { layout, node, element: { current: node }, readFrame: vi.fn(() => readSurfaceFrame(layout)) };
}
function fixture() {
  const anchor = part(30.25, 20.5);
  const primary = part(250.75, 100.25, 1.25, 0.75);
  const source: RefObject<HTMLElement | null> = { current: anchor.node };
  const props: SurfacePresenceInput = {
    parts: { kind: 'single', primary }, geometry: { kind: 'lift', origin: { kind: 'anchor', anchor: { ref: source }, unavailable: 'surface-center' } },
    open: true, present: true, enabled: true, onExited: vi.fn(),
  };
  return { anchor, primary, source, props };
}
function freeze(element: HTMLElement, time: number) {
  const animation = element.getAnimations()[0];
  expect(animation).toBeDefined(); animation.pause(); animation.currentTime = time; return animation;
}
function matrix(element: HTMLElement) { return new DOMMatrix(getComputedStyle(element).transform); }
function box(element: HTMLElement) { const b = element.getBoundingClientRect(); return [b.left, b.top, b.width, b.height]; }
function sameBox(a: number[], b: number[]) { for (const [i, value] of a.entries()) expect(value).toBeCloseTo(b[i], 3); }
async function geometryEvent(read?: ReturnType<typeof part>['readFrame']) {
  const count = read?.mock.calls.length ?? 0;
  act(() => { window.dispatchEvent(new Event('resize')); });
  if (read) await expect.poll(() => read.mock.calls.length).toBeGreaterThan(count);
  else await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
}
afterEach(async () => {
  cleanup(); document.querySelectorAll('[data-surface-fixture]').forEach(node => node.remove());
  vi.restoreAllMocks(); await commands.emulateReducedMotion(false);
});

it('maps a common viewport source into fractional portal parts on one explicit native clock', async () => {
  const { anchor, primary, props } = fixture(); const peer = part(401.375, 100.25, 0.5, 1.5);
  render(<Probe {...props} parts={{ kind: 'paired', primary, peer }} />);
  const a = freeze(primary.node, 0); const b = freeze(peer.node, 0);
  const source = anchor.node.getBoundingClientRect();
  // Independent fixed-point oracle: applying the actual local native matrix leaves the viewport source unchanged (apart from sink).
  for (const item of [primary, peer]) {
    const frame = item.readFrame()!; const f = frame.viewportFromLocal; const m = matrix(item.node);
    const local = new DOMPoint((source.left + source.width / 2 - f.x) / f.scaleX, (source.top + source.height / 2 - f.y) / f.scaleY);
    const projected = local.matrixTransform(m);
    expect(f.x + projected.x * f.scaleX).toBeCloseTo(source.left + source.width / 2, 3);
    expect(f.y + projected.y * f.scaleY).toBeCloseTo(source.top + source.height / 2 + 0.56, 3);
    expect(item.node.getAnimations()).toHaveLength(1);
  }
  expect(a.startTime).toBe(b.startTime);
  // Resume at the same timeline sample and check the real native clock, not only paused null startTime.
  a.play(); b.play(); await Promise.all([a.ready, b.ready]); expect(a.startTime).toBe(b.startTime);
});

it('inherits paint position and velocity through rapid reversal and ignores a stale finish', async () => {
  const { primary, props } = fixture(); const view = render(<Probe {...props} />);
  const old = freeze(primary.node, 80); const before = box(primary.node); const scale = matrix(primary.node).a;
  view.rerender(<Probe {...props} open={false} />);
  const closing = freeze(primary.node, 0); sameBox(box(primary.node), before);
  closing.currentTime = 5;
  expect(matrix(primary.node).a).toBeGreaterThan(scale);
  closing.finish();
  view.rerender(<Probe {...props} />);
  await Promise.resolve();
  expect(props.onExited).not.toHaveBeenCalled(); expect(primary.node.getAnimations()).toHaveLength(1);
  expect(old.playState).toBe('idle');
  view.unmount(); expect(primary.node.getAnimations()).toHaveLength(0);
});

it('rebases moving source and final frame without a paint jump then freezes detached or replaced sources', async () => {
  const { anchor, primary, source, props } = fixture(); const view = render(<Probe {...props} />);
  freeze(primary.node, 80); const before = box(primary.node);
  anchor.layout.style.left = '95.5px'; await geometryEvent(primary.readFrame); freeze(primary.node, 0); sameBox(box(primary.node), before);
  // Frame change permits ordinary layout displacement but keeps local motion contribution.
  const oldMatrix = matrix(primary.node);
  primary.layout.style.left = '300.5px'; primary.layout.style.transform = 'scale(1.5, 0.9)';
  await geometryEvent(primary.readFrame); freeze(primary.node, 0);
  expect(matrix(primary.node).e).toBeCloseTo(oldMatrix.e, 4); expect(matrix(primary.node).f).toBeCloseTo(oldMatrix.f, 4);
  anchor.layout.remove(); await geometryEvent(primary.readFrame); const fixed = primary.node.getAnimations()[0];
  const replacement = part(700, 500); source.current = replacement.node;
  await geometryEvent(primary.readFrame); expect(primary.node.getAnimations()[0]).toBe(fixed);
  // A new full cycle may acquire the replacement, but reversal in this cycle may not.
  view.rerender(<Probe {...props} open={false} />); freeze(primary.node, 0).finish();
  await act(async () => { await Promise.resolve(); });
  expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} present={false} open={false} />);
  view.rerender(<Probe {...props} />); freeze(primary.node, 0);
  expect(matrix(primary.node).e).toBeGreaterThan(0);
});

it('uses explicit center for an unavailable first source for the whole cycle', async () => {
  const { primary, source, props } = fixture(); source.current = null;
  render(<Probe {...props} />); const a = freeze(primary.node, 0);
  const before = box(primary.node); source.current = part(700, 500).node;
  await geometryEvent(primary.readFrame); expect(primary.node.getAnimations()[0]).toBe(a); sameBox(box(primary.node), before);
});

it.each(['ltr', 'rtl'])('projects paired %s edges at the same physical seam', direction => {
  const primary = part(250.75, 100.25, 1.25, 0.75); primary.node.dir = direction;
  const edge = direction === 'ltr' ? 401.375 : 250.75;
  const peer = part(edge, 100.25, 0.5, 1.5);
  render(<Probe parts={{ kind: 'paired', primary, peer }} geometry={{ kind: 'lift', origin: { kind: 'paired-edge', edge: 'inline-end' } }} open present enabled onExited={() => {}} />);
  freeze(primary.node, 0); freeze(peer.node, 0);
  const panel = primary.node.getBoundingClientRect(); const seam = peer.node.getBoundingClientRect();
  expect(direction === 'ltr' ? panel.right : panel.left).toBeCloseTo(seam.left, 3);
});

it('settles reduced motion immediately and disposes a running close once', async () => {
  const { primary, props } = fixture(); const view = render(<Probe {...props} />);
  freeze(primary.node, 80); view.rerender(<Probe {...props} open={false} />);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => primary.node.getAnimations().length).toBe(0);
  expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} open={false} />); expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} />); expect(primary.node.getAnimations()).toHaveLength(0);
  view.unmount(); await geometryEvent(); expect(primary.node.getAnimations()).toHaveLength(0);
});

it('cleans acquired effects on failed paired start and rejects foreign frames and unsupported mappings', () => {
  const { primary, props } = fixture(); const peer = part(500, 100);
  vi.spyOn(peer.node, 'animate').mockImplementation(() => { throw new Error('Native pair refused'); });
  expect(() => render(<Probe {...props} parts={{ kind: 'paired', primary, peer }} />)).toThrow('Native pair refused');
  expect(primary.node.getAnimations()).toHaveLength(0);
  const foreign = { ...primary, readFrame: () => ({ ...primary.readFrame()!, document: document.implementation.createHTMLDocument() }) };
  expect(() => render(<Probe {...props} parts={{ kind: 'single', primary: foreign }} />)).toThrow('Surface frames must share a document');
  primary.layout.style.transform = 'rotate(5deg)';
  expect(() => render(<Probe {...props} />)).toThrow('Unsupported surface transform');
  expect(primary.node.getAnimations()).toHaveLength(0);
});

it('coalesces scroll and visualViewport rebase events and removes observers/listeners without an idle frame loop', async () => {
  const { anchor, primary, props } = fixture();
  const disconnect = vi.spyOn(ResizeObserver.prototype, 'disconnect');
  const listen = vi.spyOn(primary.node.ownerDocument.defaultView!, 'addEventListener');
  const view = render(<Probe {...props} />);
  freeze(primary.node, 80);
  const request = window.requestAnimationFrame.bind(window);
  await expect.poll(() => primary.readFrame.mock.calls.length).toBeGreaterThan(1);
  const ownerWindow = primary.node.ownerDocument.defaultView!;
  const raf = vi.spyOn(ownerWindow, 'requestAnimationFrame');
  expect(listen.mock.calls.map(call => call[0])).toContain('resize');
  expect(disconnect).not.toHaveBeenCalled();
  const before = box(primary.node);
  anchor.layout.style.left = '77.25px';
  act(() => {
    anchor.layout.dispatchEvent(new Event('scroll', { bubbles: true }));
    ownerWindow.dispatchEvent(new Event('resize'));
    window.visualViewport!.dispatchEvent(new Event('resize'));
    window.visualViewport!.dispatchEvent(new Event('scroll'));
  });
  expect(raf).toHaveBeenCalledOnce();
  await new Promise<void>(resolve => request(() => resolve()));
  freeze(primary.node, 0); sameBox(box(primary.node), before);
  await new Promise<void>(resolve => request(() => request(() => resolve())));
  expect(raf).toHaveBeenCalledOnce();
  view.unmount(); expect(disconnect).toHaveBeenCalledOnce();
  raf.mockClear();
  act(() => { document.dispatchEvent(new Event('scroll')); window.dispatchEvent(new Event('resize')); window.visualViewport!.dispatchEvent(new Event('scroll')); });
  expect(raf).not.toHaveBeenCalled(); expect(primary.node.getAnimations()).toHaveLength(0);
});

it.each(['primary', 'peer', 'both'].flatMap(which => [false, true].map(moveOrigin => ({ which, moveOrigin }))))('preserves paired local contribution and analytical velocity: $which frames, origin=$moveOrigin', ({ which, moveOrigin }) => {
  const { anchor, primary, props } = fixture(); const peer = part(450.375, 180.25, 0.5, 1.5);
  const paired = { ...props, parts: { kind: 'paired' as const, primary, peer } };
  const view = render(<Probe {...paired} />);
  const items = [primary, peer];
  const oldTrajectory = activeTrajectory();
  const anchorBox = anchor.node.getBoundingClientRect();
  const oldOrigin = { x: anchorBox.left + anchorBox.width / 2, y: anchorBox.top + anchorBox.height / 2 };
  const before = items.map((item, index) => {
    freeze(item.node, 80); const m = matrix(item.node);
    return { m, velocity: localVelocity(oldTrajectory.sample(80) as Sample, item.readFrame()!, oldOrigin, index) };
  });
  if (which !== 'peer') { primary.layout.style.left = '750.75px'; primary.layout.style.transform = 'scale(0.8, 1.3)'; }
  if (which !== 'primary') { peer.layout.style.left = '380.5px'; peer.layout.style.top = '210.5px'; peer.layout.style.transform = 'scale(1.7, 0.6)'; }
  if (moveOrigin) anchor.layout.style.left = '140.5px';
  view.rerender(<Probe {...paired} />);
  for (const [index, item] of items.entries()) {
    const animation = freeze(item.node, 0); const m = matrix(item.node); const old = before[index];
    expect(m.e).toBeCloseTo(old.m.e, 4); expect(m.f).toBeCloseTo(old.m.f, 4); expect(m.a).toBeCloseTo(old.m.a, 4);
    const frame = item.readFrame()!; const rect = item.node.getBoundingClientRect();
    expect(rect.left).toBeCloseTo(frame.viewportFromLocal.x + old.m.e * frame.viewportFromLocal.scaleX, 3);
    expect(rect.top).toBeCloseTo(frame.viewportFromLocal.y + old.m.f * frame.viewportFromLocal.scaleY, 3);
    const nextBox = anchor.node.getBoundingClientRect();
    const velocity = localVelocity(activeTrajectory().sample(0) as Sample, frame, { x: nextBox.left + nextBox.width / 2, y: nextBox.top + nextBox.height / 2 }, index);
    expect(velocity.x).toBeCloseTo(old.velocity.x, 8); expect(velocity.y).toBeCloseTo(old.velocity.y, 8);
    animation.currentTime = 1; const after = matrix(item.node);
    // The native 10ms rendering grid approximates the same analytical derivative.
    expect(Math.abs((after.e - m.e) / 0.001 - velocity.x)).toBeLessThan(Math.abs(velocity.x) * 0.1 + 0.1);
  }
  expect(primary.node.getAnimations()).toHaveLength(1); expect(peer.node.getAnimations()).toHaveLength(1);
});

it.each(['ref', 'frame'].flatMap(kind => [false, true].map(open => ({ kind, open }))))('resumes sampled presence after null $kind (open=$open) with unique completion and stale-finish rejection', async ({ kind, open }) => {
  const { primary, props } = fixture(); const read = primary.readFrame.getMockImplementation()!;
  const view = render(<Probe {...props} />); freeze(primary.node, 80);
  view.rerender(<Probe {...props} open={open} />);
  const closing = freeze(primary.node, 20); const before = box(primary.node);
  const sampled = activeTrajectory().sample(20) as Sample;
  if (kind === 'ref') primary.element.current = null as unknown as HTMLDivElement;
  else primary.readFrame.mockImplementation(() => null);
  view.rerender(<Probe {...props} open={open} />);
  view.rerender(<Probe {...props} open={open} />);
  expect(primary.node.getAnimations()).toHaveLength(0); expect(props.onExited).not.toHaveBeenCalled();
  primary.element.current = primary.node; primary.readFrame.mockImplementation(read);
  view.rerender(<Probe {...props} open={open} />);
  const resumed = freeze(primary.node, 0); sameBox(box(primary.node), before); expect(closing.playState).toBe('idle');
  const restored = activeTrajectory().sample(0) as Sample;
  expect(restored.value.progress).toBeCloseTo(sampled.value.progress, 10);
  expect(restored.velocity.progress).toBeCloseTo(sampled.velocity.progress, 10);
  expect(restored.value.offsets).toEqual(sampled.value.offsets); expect(restored.velocity.offsets).toEqual(sampled.velocity.offsets);
  if (open) { view.rerender(<Probe {...props} open={false} />); freeze(primary.node, 0).finish(); }
  else resumed.finish();
  view.rerender(<Probe {...props} />); await act(async () => { await Promise.resolve(); });
  expect(props.onExited).not.toHaveBeenCalled();
  freeze(primary.node, 20); view.rerender(<Probe {...props} open={false} />); freeze(primary.node, 0).finish();
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} open={false} />); expect(props.onExited).toHaveBeenCalledOnce();
});

it('waits for a preparation frame without claiming an exit', () => {
  const { primary, props } = fixture(); primary.readFrame.mockImplementation(() => null);
  const view = render(<Probe {...props} open={false} />);
  view.rerender(<Probe {...props} open={false} />);
  expect(props.onExited).not.toHaveBeenCalled(); expect(primary.node.getAnimations()).toHaveLength(0);
});


it.each(['ref', 'frame'])('suspends the whole paired clock when the peer %s is null and resumes both', kind => {
  const { primary, props } = fixture(); const peer = part(450, 100, 0.5, 1.5);
  const paired = { ...props, parts: { kind: 'paired' as const, primary, peer } };
  const read = peer.readFrame.getMockImplementation()!; const view = render(<Probe {...paired} />);
  freeze(primary.node, 80); freeze(peer.node, 80);
  view.rerender(<Probe {...paired} open={false} />);
  freeze(primary.node, 20); freeze(peer.node, 20); const before = [box(primary.node), box(peer.node)];
  if (kind === 'ref') peer.element.current = null as unknown as HTMLDivElement;
  else peer.readFrame.mockImplementation(() => null);
  view.rerender(<Probe {...paired} open={false} />);
  expect(primary.node.getAnimations()).toHaveLength(0); expect(peer.node.getAnimations()).toHaveLength(0);
  peer.element.current = peer.node; peer.readFrame.mockImplementation(read);
  view.rerender(<Probe {...paired} open={false} />);
  freeze(primary.node, 0); freeze(peer.node, 0);
  sameBox(box(primary.node), before[0]); sameBox(box(peer.node), before[1]);
});


it('attaches geometry observation after a null preparation ref becomes available', async () => {
  const { anchor, primary, props } = fixture(); primary.element.current = null as unknown as HTMLDivElement;
  const view = render(<Probe {...props} />); expect(primary.node.getAnimations()).toHaveLength(0);
  expect(props.onExited).not.toHaveBeenCalled(); primary.element.current = primary.node;
  view.rerender(<Probe {...props} />); freeze(primary.node, 80); const before = box(primary.node);
  anchor.layout.style.left = '90px'; await geometryEvent(primary.readFrame);
  freeze(primary.node, 0); sameBox(box(primary.node), before);
});

it('settles a suspended close once when reduced motion is enabled before frame recovery', async () => {
  const { primary, props } = fixture(); const read = primary.readFrame.getMockImplementation()!;
  const view = render(<Probe {...props} />); freeze(primary.node, 80);
  view.rerender(<Probe {...props} open={false} />); freeze(primary.node, 20);
  primary.readFrame.mockImplementation(() => null); view.rerender(<Probe {...props} open={false} />);
  await commands.emulateReducedMotion(true); expect(props.onExited).not.toHaveBeenCalled();
  primary.readFrame.mockImplementation(read); view.rerender(<Probe {...props} open={false} />);
  expect(primary.node.getAnimations()).toHaveLength(0); expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} open={false} />); expect(props.onExited).toHaveBeenCalledOnce();
});

it('keeps shared progress when a declaration gains or releases a peer', () => {
  const { primary, props } = fixture(); const peer = part(450, 100, 0.5, 1.5);
  const view = render(<Probe {...props} />); freeze(primary.node, 80); const before = box(primary.node);
  view.rerender(<Probe {...props} parts={{ kind: 'paired', primary, peer }} />);
  freeze(primary.node, 0); freeze(peer.node, 0); sameBox(box(primary.node), before);
  expect(matrix(peer.node).a).toBeCloseTo(matrix(primary.node).a, 5);
  view.rerender(<Probe {...props} />); freeze(primary.node, 0); sameBox(box(primary.node), before);
  expect(peer.node.getAnimations()).toHaveLength(0);
});

// Real React refs are cleared and reassigned within the same commit: the hook
// must see element identity without requiring an intermediate null render.
type KeyedPart = { id: string; x: number; y: number; sx: number; sy: number };
function KeyedProbe({ items, ready = true, ...input }: Omit<SurfacePresenceInput, 'parts'> & { items: KeyedPart[]; ready?: boolean }) {
  const primary = useRef<HTMLDivElement>(null); const peer = useRef<HTMLDivElement>(null);
  const primaryLayout = useRef<HTMLDivElement>(null); const peerLayout = useRef<HTMLDivElement>(null);
  const a = { element: primary, readFrame: () => ready ? readSurfaceFrame(primaryLayout.current) : null };
  const b = { element: peer, readFrame: () => ready ? readSurfaceFrame(peerLayout.current) : null };
  useSurfacePresence({ ...input, parts: items.length === 1 ? { kind: 'single', primary: a } : { kind: 'paired', primary: a, peer: b } });
  return <>{items.map((item, index) => <div key={item.id} ref={index === 0 ? primaryLayout : peerLayout}
    style={{ position: 'fixed', left: item.x, top: item.y, width: 120.5, height: 60.25, transformOrigin: '0 0', transform: `scale(${item.sx},${item.sy})` }}>
    <div data-keyed-surface={item.id} ref={index === 0 ? primary : peer} style={{ width: '100%', height: '100%' }} />
  </div>)}</>;
}
function keyedNode(id: string): HTMLElement { return (Array.from(document.querySelectorAll<HTMLElement>('[data-keyed-surface]')).find(node => node.dataset.keyedSurface === id) ?? null)!; }
function keyedItems() { return [{ id: 'a', x: 250.75, y: 100.25, sx: 1.25, sy: 0.75 }, { id: 'b', x: 450.375, y: 180.25, sx: 0.5, sy: 1.5 }]; }
const replacements = Object.freeze(['single', 'primary', 'peer', 'both'].flatMap(which => [false, true].flatMap(newFrame =>
  ['entry', 'closing', 'reversal'].map(phase => ({ which, newFrame, phase })))));
it.each(replacements)('transfers keyed $which replacement: newFrame=$newFrame during $phase', async ({ which, newFrame, phase }) => {
  const { props, anchor } = fixture(); let items = keyedItems().slice(0, which === 'single' ? 1 : 2);
  const view = render(<KeyedProbe {...props} items={items} />);
  items.forEach(item => freeze(keyedNode(item.id), 80));
  if (phase !== 'entry') {
    view.rerender(<KeyedProbe {...props} items={items} open={false} />);
    items.forEach(item => freeze(keyedNode(item.id), 20));
  }
  const open = phase !== 'closing';
  const trajectory = activeTrajectory(); const sample = trajectory.sample(phase === 'entry' ? 80 : 20) as Sample;
  const rect = anchor.node.getBoundingClientRect(); const origin = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  const before = items.map((item, index) => {
    const node = keyedNode(item.id); const frame = readSurfaceFrame(node.parentElement)!;
    return { node, animation: node.getAnimations()[0], matrix: matrix(node), velocity: localVelocity(sample, frame, origin, index) };
  });
  items = items.map((item, index) => which === 'both' || which === 'single' || (which === 'primary' ? index === 0 : index === 1)
    ? { ...item, id: item.id + '-new', ...(newFrame ? { x: item.x + 90.5, y: item.y + 20.25, sx: item.sx * 1.3, sy: item.sy * 0.8 } : {}) } : item);
  view.rerender(<KeyedProbe {...props} items={items} open={open} />);
  const current = items.map(item => keyedNode(item.id));
  expect(typeof current[0].getAnimations()[0].startTime).toBe('number');
  expect(new Set(current.map(node => node.getAnimations()[0].startTime)).size).toBe(1);
  for (const [index, node] of current.entries()) {
    const old = before[index]; expect(old.animation.playState).toBe('idle');
    if (node !== old.node) { expect(old.node.isConnected).toBe(false); expect(old.node.getAnimations()).toHaveLength(0); }
    freeze(node, 0); const m = matrix(node);
    expect(m.a).toBeCloseTo(old.matrix.a, 4); expect(m.e).toBeCloseTo(old.matrix.e, 4); expect(m.f).toBeCloseTo(old.matrix.f, 4);
    const next = activeTrajectory().sample(0) as Sample;
    expect(next.value.progress).toBeCloseTo(sample.value.progress, 10); expect(next.velocity.progress).toBeCloseTo(sample.velocity.progress, 10);
    const velocity = localVelocity(next, readSurfaceFrame(node.parentElement)!, origin, index);
    expect(velocity.x).toBeCloseTo(old.velocity.x, 8); expect(velocity.y).toBeCloseTo(old.velocity.y, 8);
  }
  // An obsolete disconnected Animation can be finished by an external queued
  // callback, but it has no completion authority over this surface group.
  before.forEach(old => old.animation.finish());
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).not.toHaveBeenCalled();
  before.forEach(old => old.animation.cancel());
  if (open) { view.rerender(<KeyedProbe {...props} items={items} open={false} />); current.forEach(node => freeze(node, 0)); }
  if (current.length === 2) {
    current[0].getAnimations()[0].finish(); await act(async () => { await Promise.resolve(); });
    expect(props.onExited).not.toHaveBeenCalled();
  }
  current.forEach(node => node.getAnimations()[0]?.finish());
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<KeyedProbe {...props} items={items} open={false} />); expect(props.onExited).toHaveBeenCalledOnce();
});

it.each(['single', 'paired'])('binds keyed %s preparation and settled replacements without replaying entry', async kind => {
  const { props } = fixture(); let items = keyedItems().slice(0, kind === 'single' ? 1 : 2);
  const view = render(<KeyedProbe {...props} items={items} ready={false} />);
  items = items.map(item => ({ ...item, id: item.id + '-ready' }));
  view.rerender(<KeyedProbe {...props} items={items} ready={false} />);
  expect(props.onExited).not.toHaveBeenCalled(); items.forEach(item => expect(keyedNode(item.id).getAnimations()).toHaveLength(0));
  view.rerender(<KeyedProbe {...props} items={items} />);
  items.forEach(item => freeze(keyedNode(item.id), 0).finish());
  await act(async () => { await Promise.resolve(); });
  items = items.map(item => ({ ...item, id: item.id + '-settled' }));
  view.rerender(<KeyedProbe {...props} items={items} />);
  items.forEach(item => { expect(keyedNode(item.id).getAnimations()).toHaveLength(0); expect(matrix(keyedNode(item.id)).isIdentity).toBe(true); });
  view.rerender(<KeyedProbe {...props} items={items} open={false} />);
  items.forEach(item => freeze(keyedNode(item.id), 0).finish());
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).toHaveBeenCalledOnce();
});

it.each([false, true])('moves per-part compensation with retained keyed elements across order and cardinality changes (equalFrames=%s)', equalFrames => {
  const { props, anchor } = fixture(); let items = keyedItems();
  const view = render(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 80));
  // Create distinct nonzero per-part offsets before permuting the declarations.
  items = items.map((item, i) => equalFrames ? { ...item, x: 350.5, y: 210.25, sx: 0.8, sy: 1.3 }
    : { ...item, x: item.x + (i ? -30 : 70), sx: item.sx * (i ? 0.8 : 1.4) });
  view.rerender(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 20));
  const sample = activeTrajectory().sample(20) as Sample;
  const rect = anchor.node.getBoundingClientRect(); const origin = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
  const before = items.map((item, index) => ({ node: keyedNode(item.id), matrix: matrix(keyedNode(item.id)),
    velocity: localVelocity(sample, readSurfaceFrame(keyedNode(item.id).parentElement)!, origin, index) }));
  items = [items[1], items[0]];
  view.rerender(<KeyedProbe {...props} items={items} />);
  for (const [index, item] of items.entries()) {
    const old = before[1 - index]; const node = keyedNode(item.id); expect(node).toBe(old.node); freeze(node, 0);
    expect(matrix(node).e).toBeCloseTo(old.matrix.e, 4); expect(matrix(node).f).toBeCloseTo(old.matrix.f, 4);
    const velocity = localVelocity(activeTrajectory().sample(0) as Sample, readSurfaceFrame(node.parentElement)!, origin, index);
    expect(velocity.x).toBeCloseTo(old.velocity.x, 8); expect(velocity.y).toBeCloseTo(old.velocity.y, 8);
  }
  items = [items[1]]; const old = before[0];
  view.rerender(<KeyedProbe {...props} items={items} />); freeze(old.node, 0);
  expect(matrix(old.node).e).toBeCloseTo(old.matrix.e, 4); expect(before[1].node.getAnimations()).toHaveLength(0);
});

it('rejects a queued obsolete paired finish after a same-commit keyed replacement', async () => {
  const { props } = fixture(); let items = keyedItems(); const view = render(<KeyedProbe {...props} items={items} />);
  items.forEach(item => freeze(keyedNode(item.id), 80));
  view.rerender(<KeyedProbe {...props} items={items} open={false} />);
  const old = items.map(item => freeze(keyedNode(item.id), 20));
  act(() => {
    old.forEach(animation => animation.finish());
    items = items.map(item => ({ ...item, id: item.id + '-queued' }));
    view.rerender(<KeyedProbe {...props} items={items} open={true} />);
  });
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).not.toHaveBeenCalled();
  items.forEach(item => { freeze(keyedNode(item.id), 0); });
  view.rerender(<KeyedProbe {...props} items={items} open={false} />);
  items.forEach(item => freeze(keyedNode(item.id), 0).finish());
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).toHaveBeenCalledOnce();
});

it('rebinds observers and resumes a replacement group from a suspended sample', () => {
  const { props } = fixture(); let items = keyedItems(); const observe = vi.spyOn(ResizeObserver.prototype, 'observe');
  const view = render(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 80));
  view.rerender(<KeyedProbe {...props} items={items} open={false} />);
  items.forEach(item => freeze(keyedNode(item.id), 20)); const before = items.map(item => matrix(keyedNode(item.id)));
  view.rerender(<KeyedProbe {...props} items={items} open={false} ready={false} />);
  const oldNodes = items.map(item => keyedNode(item.id));
  items = items.map(item => ({ ...item, id: item.id + '-suspended' }));
  view.rerender(<KeyedProbe {...props} items={items} open={false} ready={false} />);
  expect(props.onExited).not.toHaveBeenCalled();
  view.rerender(<KeyedProbe {...props} items={items} open={false} />);
  for (const [i, item] of items.entries()) {
    const node = keyedNode(item.id); freeze(node, 0);
    expect(matrix(node).e).toBeCloseTo(before[i].e, 4); expect(matrix(node).f).toBeCloseTo(before[i].f, 4);
    expect(observe.mock.calls.some(([element]) => element === node)).toBe(true); expect(oldNodes[i].getAnimations()).toHaveLength(0);
  }
  view.unmount(); items.forEach(item => expect(keyedNode(item.id)).toBeNull());
});

it.each(['single', 'paired'])('settles disabled keyed %s replacements and releases old group effects', async kind => {
  const { props } = fixture(); let items = keyedItems().slice(0, kind === 'single' ? 1 : 2);
  const view = render(<KeyedProbe {...props} items={items} />);
  const old = items.map(item => freeze(keyedNode(item.id), 80));
  items = items.map(item => ({ ...item, id: item.id + '-disabled' }));
  view.rerender(<KeyedProbe {...props} items={items} open={false} enabled={false} />);
  old.forEach(animation => expect(animation.playState).toBe('idle'));
  items.forEach(item => expect(keyedNode(item.id).getAnimations()).toHaveLength(0));
  expect(props.onExited).toHaveBeenCalledOnce();
  view.rerender(<KeyedProbe {...props} items={items} open={false} enabled={false} />);
  await act(async () => { await Promise.resolve(); }); expect(props.onExited).toHaveBeenCalledOnce();
});

it.each(['replace', 'move-and-replace', 'gain-peer'])('maps nonzero part channels on keyed $0', transition => {
  const { props } = fixture(); let items = keyedItems().slice(0, transition === 'gain-peer' ? 1 : 2);
  const view = render(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 80));
  items = items.map((item, i) => ({ ...item, x: item.x + (i ? -45 : 80), sx: item.sx * (i ? 1.4 : 0.8) }));
  view.rerender(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 20));
  const sample = activeTrajectory().sample(20) as Sample;
  const before = items.map(item => matrix(keyedNode(item.id)));
  if (transition === 'gain-peer') items = [...items, keyedItems()[1]];
  else if (transition === 'move-and-replace') items = [items[1], { ...items[0], id: 'c' }];
  else items = items.map(item => ({ ...item, id: item.id + '-offset' }));
  view.rerender(<KeyedProbe {...props} items={items} />); items.forEach(item => freeze(keyedNode(item.id), 0));
  const next = activeTrajectory().sample(0) as Sample;
  expect(next.value.progress).toBeCloseTo(sample.value.progress, 10); expect(next.velocity.progress).toBeCloseTo(sample.velocity.progress, 10);
  if (transition === 'replace') {
    for (const [index, item] of items.entries()) {
      expect(matrix(keyedNode(item.id)).e).toBeCloseTo(before[index].e, 4);
      expect(next.value.offsets[index].x).toBeCloseTo(sample.value.offsets[index].x, 10);
      expect(next.velocity.offsets[index].y).toBeCloseTo(sample.velocity.offsets[index].y, 10);
    }
  } else {
    const retained = transition === 'gain-peer' ? 0 : 1;
    expect(matrix(keyedNode(items[0].id)).e).toBeCloseTo(before[retained].e, 4);
    expect(next.value.offsets[1]).toEqual({ x: 0, y: 0 }); expect(next.velocity.offsets[1]).toEqual({ x: 0, y: 0 });
  }
});

it.each(['detached', 'ref-replaced'])('rejects finish from a $0 surface before another layout commit', async kind => {
  const { primary, props } = fixture(); const view = render(<Probe {...props} />); freeze(primary.node, 80);
  view.rerender(<Probe {...props} open={false} />); const closing = freeze(primary.node, 20);
  if (kind === 'detached') primary.node.remove(); else primary.element.current = part(250.75, 100.25).node;
  closing.finish(); await act(async () => { await Promise.resolve(); });
  expect(props.onExited).not.toHaveBeenCalled(); expect(primary.node.getAnimations()).toHaveLength(0);
  primary.element.current = primary.node; primary.layout.append(primary.node);
  view.rerender(<Probe {...props} open={false} />);
  expect(props.onExited).toHaveBeenCalledOnce();
});


it('checks current declaration refs when equal nodes did not require a new playback', async () => {
  const { primary, props } = fixture(); const view = render(<Probe {...props} />); freeze(primary.node, 80);
  view.rerender(<Probe {...props} open={false} />); const closing = freeze(primary.node, 20);
  const next = { ...primary, element: { current: primary.node } };
  view.rerender(<Probe {...props} open={false} parts={{ kind: 'single', primary: next }} />);
  expect(primary.node.getAnimations()[0]).toBe(closing);
  next.element.current = part(250.75, 100.25).node;
  closing.finish(); await act(async () => { await Promise.resolve(); }); expect(props.onExited).not.toHaveBeenCalled();
  next.element.current = primary.node;
  view.rerender(<Probe {...props} open={false} parts={{ kind: 'single', primary: next }} />);
  expect(props.onExited).toHaveBeenCalledOnce();
});

it('reads current binding through native phases scroll scale null and replacement', async () => {
  const { primary, props } = fixture();
  primary.node.style.overflow = 'auto';
  const flow = primary.node.appendChild(document.createElement('div'));
  flow.style.cssText = 'position:relative;width:300px;height:400px';
  const source = flow.appendChild(document.createElement('span'));
  source.style.cssText = 'position:absolute;left:23.25px;top:70.5px;width:40.25px;height:19.5px';
  let port: ReturnType<typeof useSurfacePresence>;
  function Reader(input: SurfacePresenceInput) { port = useSurfacePresence(input); return null; }
  const view = render(<Reader {...props} />);
  primary.node.scrollTop = 29;
  const expected = primary.readFrame();
  const paints = new Set<string>();
  for (const time of [0, 40, 80]) {
    freeze(primary.node, time);
    const binding = port!.readBinding(); expect(binding.kind).toBe('ready');
    if (binding.kind === 'ready') expect(binding.parts).toEqual([{ element: primary.node, frame: expected }]);
    paints.add(JSON.stringify(box(source)));
  }
  expect(paints.size).toBe(3);
  primary.node.getAnimations()[0].finish(); await Promise.resolve();
  expect(port!.readBinding().kind).toBe('ready');
  view.rerender(<Reader {...props} parts={{ kind: 'single', primary: { ...primary, readFrame: () => null } }} />);
  expect(port!.readBinding().kind).toBe('unavailable');
  await commands.emulateReducedMotion(true);
  view.rerender(<Reader {...props} />);
  const reduced = port!.readBinding(); expect(reduced.kind).toBe('ready');
  if (reduced.kind === 'ready') expect(reduced.parts).toEqual([{ element: primary.node, frame: expected }]);
  expect(primary.node.getAnimations()).toHaveLength(0);
  const replacement = part(401.375, 100.25, 0.5, 1.5);
  view.rerender(<Reader {...props} parts={{ kind: 'single', primary: replacement }} />);
  const replaced = port!.readBinding(); expect(replaced.kind).toBe('ready');
  if (replaced.kind === 'ready') expect(replaced.parts).toEqual([{ element: replacement.node, frame: replacement.readFrame() }]);
});


it('restores geometry observation after StrictMode layout effect replay', async () => {
  const { anchor, primary, props } = fixture();
  render(<StrictMode><Probe {...props} /></StrictMode>);
  anchor.layout.style.left = '105.75px';
  await geometryEvent(primary.readFrame);
  expect(primary.node.getAnimations()).toHaveLength(1);
});

it('stops visual pumping on suspension reduction unmount and stale completion', async () => {
  const { primary, props } = fixture(); const notices = vi.fn();
  const view = render(<Probe {...props} onVisualChange={notices} />);
  await geometryEvent(); await new Promise(requestAnimationFrame);
  expect(notices.mock.calls.length).toBeGreaterThan(0);
  const stale = primary.node.getAnimations()[0];
  view.rerender(<Probe {...props} onVisualChange={notices} parts={{ kind: 'single', primary: { ...primary, readFrame: () => null } }} />);
  await geometryEvent(); notices.mockClear();
  const request = vi.spyOn(window, 'requestAnimationFrame');
  const idle = async () => {
    request.mockClear();
    const mutations: MutationRecord[] = []; const observer = new MutationObserver(records => mutations.push(...records));
    observer.observe(primary.layout, { attributes: true, subtree: true });
    for (let i = 0; i < 20; i++) await new Promise(requestAnimationFrame);
    observer.disconnect(); expect(request).toHaveBeenCalledTimes(20); expect(mutations).toEqual([]); expect(notices).not.toHaveBeenCalled();
  };
  stale.currentTime = 40; stale.play(); await stale.ready;
  await idle(); stale.cancel();
  view.rerender(<Probe {...props} onVisualChange={notices} />);
  await geometryEvent();
  const reducedStale = primary.node.getAnimations()[0];
  await commands.emulateReducedMotion(true); await geometryEvent(); notices.mockClear();
  reducedStale.currentTime = 60; reducedStale.play(); await reducedStale.ready;
  await idle(); reducedStale.cancel(); expect(primary.node.getAnimations()).toHaveLength(0);
  await commands.emulateReducedMotion(false);
  view.rerender(<Probe {...props} open={false} onVisualChange={notices} />); await geometryEvent();
  const closing = primary.node.getAnimations()[0];
  view.rerender(<Probe {...props} onVisualChange={notices} />);
  stale.dispatchEvent(new Event('finish')); closing?.dispatchEvent(new Event('finish'));
  view.unmount(); notices.mockClear(); await idle(); expect(primary.node.getAnimations()).toHaveLength(0);
});

it('switches visual callbacks on the current binding without reacquisition or idle work', async () => {
  const { primary, props } = fixture(); const first = vi.fn(); const second = vi.fn();
  const view = render(<Probe {...props} />);
  const animation = primary.node.getAnimations()[0]; animation.pause(); await animation.ready;
  const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
  await frames(3); const effect = animation.effect; const finished = animation.finished;
  expect(Object.hasOwn(animation, 'play')).toBe(false);
  view.rerender(<Probe {...props} onVisualChange={first} />); await frames(3); expect(first).toHaveBeenCalledOnce();
  first.mockClear(); view.rerender(<Probe {...props} onVisualChange={second} />);
  animation.currentTime = 80; await frames(3); expect(first).not.toHaveBeenCalled(); expect(second).toHaveBeenCalledOnce();
  view.rerender(<Probe {...props} />); second.mockClear();
  animation.currentTime = 120; animation.playbackRate = 0.1; animation.play(); await animation.ready;
  const request = vi.spyOn(window, 'requestAnimationFrame'); await frames(20);
  expect(request).toHaveBeenCalledTimes(20); expect(second).not.toHaveBeenCalled();
  expect(primary.node.getAnimations()[0]).toBe(animation); expect(animation.effect).toBe(effect); expect(animation.finished).toBe(finished);
  view.unmount(); animation.play(); await animation.ready; request.mockClear();
  await frames(20); expect(request).toHaveBeenCalledTimes(20); expect(second).not.toHaveBeenCalled(); animation.cancel();
});

it('revokes captured native controls across StrictMode replay and destruction', async () => {
  const { primary, props } = fixture(); const notices = vi.fn(); const acquired: Animation[] = [];
  const native = primary.node.animate.bind(primary.node);
  vi.spyOn(primary.node, 'animate').mockImplementation((...args) => { const animation = native(...args); acquired.push(animation); return animation; });
  const view = render(<StrictMode><Probe {...props} onVisualChange={notices} /></StrictMode>);
  expect(acquired.length).toBeGreaterThan(1);
  const stale = acquired[0]; const live = primary.node.getAnimations()[0];
  live.pause(); await live.ready;
  for (let i = 0; i < 3; i++) await new Promise(requestAnimationFrame);
  notices.mockClear(); stale.currentTime = 40; stale.play(); await stale.ready;
  for (let i = 0; i < 3; i++) await new Promise(requestAnimationFrame);
  expect(notices).not.toHaveBeenCalled(); stale.cancel();
  view.unmount(); notices.mockClear(); live.currentTime = 80; live.play(); await live.ready;
  const request = vi.spyOn(window, 'requestAnimationFrame');
  for (let i = 0; i < 20; i++) await new Promise(requestAnimationFrame);
  expect(request).toHaveBeenCalledTimes(20); expect(notices).not.toHaveBeenCalled(); live.cancel();
});
