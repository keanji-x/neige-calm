import { act, cleanup, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useEdgePresence, type EdgePresenceInput } from './edge-presence.ts';
import type { SurfaceFrame } from './geometry.ts';

function Probe(props: EdgePresenceInput) { useEdgePresence(props); return null; }
const hosts: HTMLElement[] = [];
afterEach(() => { cleanup(); hosts.splice(0).forEach(host => host.remove()); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

// Only missing JSDOM platform services are controlled here. Hook, geometry
// validation, Motion trajectory, playback target and Animation shim stay real.
function realm(view: Window, document: Document) {
  const preference = new EventTarget();
  Object.defineProperty(preference, 'matches', { configurable: true, value: false });
  Object.defineProperty(view, 'matchMedia', { configurable: true, value: vi.fn(() => preference) });
  const queued = new Map<number, FrameRequestCallback>();
  let serial = 0;
  const request = vi.spyOn(view, 'requestAnimationFrame').mockImplementation(callback => { queued.set(++serial, callback); return serial; });
  const cancel = vi.spyOn(view, 'cancelAnimationFrame').mockImplementation(id => { queued.delete(id); });
  // The repository installs the Animation shim only in the initial realm.
  if (!('timeline' in document)) Object.defineProperty(document, 'timeline', { configurable: true, value: { currentTime: 0 } });
  return { view, document, preference, queued, request, cancel,
    flush: () => { const callbacks = [...queued.values()]; queued.clear(); callbacks.forEach(callback => callback(0)); },
    resize: () => { view.dispatchEvent(new Event('resize')); },
    scroll: () => { document.dispatchEvent(new Event('scroll')); },
    reduce: () => { preference.dispatchEvent(new Event('change')); },
  };
}
function observers() {
  const records: { callback: ResizeObserverCallback; observe: ReturnType<typeof vi.fn<(node: Element) => void>>; disconnect: ReturnType<typeof vi.fn<() => void>> }[] = [];
  vi.stubGlobal('ResizeObserver', class {
    private record: (typeof records)[number];
    constructor(callback: ResizeObserverCallback) { this.record = { callback, observe: vi.fn(), disconnect: vi.fn() }; records.push(this.record); }
    observe(node: Element) { this.record.observe(node); }
    disconnect() { this.record.disconnect(); }
  });
  return records;
}
function fixture() {
  const frame = document.createElement('div'); const node = frame.appendChild(document.createElement('div'));
  document.body.append(frame); hosts.push(frame);
  let available: SurfaceFrame | null | Error | 'live' = 'live';
  const readFrame = vi.fn(() => {
    if (available instanceof Error) throw available;
    return available === 'live' ? { document: frame.ownerDocument, size: { width: 240, height: 160 },
      viewportFromLocal: { x: 0, y: 0, scaleX: 1.25, scaleY: 0.75 } } : available;
  });
  const props: EdgePresenceInput = { frame: { ref: { current: frame } }, part: { element: { current: node }, readFrame },
    edge: 'inline-end', open: true, present: true, enabled: true, onExited: vi.fn() };
  const animate = vi.spyOn(node, 'animate');
  const latest = () => animate.mock.results.at(-1)!.value as Animation;
  return { frame, node, props, readFrame, latest, set: (next: typeof available) => { available = next; } };
}
function secondRealm() {
  const iframe = document.createElement('iframe'); document.body.append(iframe); hosts.push(iframe);
  return realm(iframe.contentWindow!, iframe.contentDocument!);
}
function startClose(f: ReturnType<typeof fixture>, view: ReturnType<typeof render>) {
  f.latest().currentTime = 60;
  view.rerender(<Probe {...f.props} open={false} />);
  return f.latest();
}
const drain = async () => { await act(async () => { await Promise.resolve(); await Promise.resolve(); }); };

it('rebinds the same paint and frame refs after document adoption and revokes old deliveries', async () => {
  const a = realm(window, document); const b = secondRealm(); const records = observers(); const f = fixture();
  const oldRemove = vi.spyOn(a.document, 'removeEventListener'); const newRemove = vi.spyOn(b.document, 'removeEventListener');
  const view = render(<Probe {...f.props} />); const oldAnimation = startClose(f, view);
  a.resize(); const stale = [...a.queued.values()]; expect(stale).toHaveLength(1);
  b.document.body.append(b.document.adoptNode(f.frame));
  view.rerender(<Probe {...f.props} open={false} />);
  expect(f.props.frame.ref.current).toBe(f.frame); expect(f.props.part.element.current).toBe(f.node);
  expect(f.node.ownerDocument).toBe(b.document); expect(f.readFrame()!.document.defaultView).toBe(b.view);
  expect(records[0].disconnect).toHaveBeenCalledTimes(1);
  expect(records[1].observe.mock.calls.map(call => call[0])).toEqual([f.node, f.frame]);
  expect(a.cancel).toHaveBeenCalledTimes(1); expect(a.queued.size).toBe(0);
  expect(oldRemove).toHaveBeenCalledWith('scroll', expect.any(Function), true);
  expect(newRemove).not.toHaveBeenCalledWith('scroll', expect.any(Function), true);
  expect(oldAnimation.playState).toBe('idle');
  const reads = f.readFrame.mock.calls.length;
  a.resize(); a.scroll(); a.reduce(); records[0].callback([], {} as ResizeObserver); stale.forEach(callback => callback(0)); a.flush();
  expect(f.readFrame).toHaveBeenCalledTimes(reads);
  b.resize(); expect(b.queued.size).toBe(1); b.flush(); expect(f.readFrame).toHaveBeenCalledTimes(reads + 1);
  b.scroll(); b.flush(); expect(f.readFrame).toHaveBeenCalledTimes(reads + 2);
  b.reduce(); expect(f.readFrame).toHaveBeenCalledTimes(reads + 3);
  oldAnimation.finish(); await drain(); expect(f.props.onExited).not.toHaveBeenCalled();
  const current = f.latest(); Object.defineProperty(b.preference, 'matches', { value: true }); b.reduce();
  expect(current.playState).toBe('idle'); expect(f.props.onExited).toHaveBeenCalledTimes(1);
  b.resize(); const pending = [...b.queued.values()]; view.unmount();
  expect(records[1].disconnect).toHaveBeenCalledTimes(1); expect(b.queued.size).toBe(0);
  expect(newRemove).toHaveBeenCalledWith('scroll', expect.any(Function), true);
  const released = f.readFrame.mock.calls.length;
  b.resize(); b.scroll(); b.reduce(); records[1].callback([], {} as ResizeObserver); pending.forEach(callback => callback(0)); b.flush();
  expect(f.readFrame).toHaveBeenCalledTimes(released);
});

it('releases observations on ref/node/null/present changes and recovers unavailable frames', async () => {
  const a = realm(window, document); const records = observers(); const f = fixture();
  const view = render(<Probe {...f.props} />); const close = startClose(f, view);
  const props = { ...f.props, frame: { ref: { current: f.frame as HTMLElement | null } },
    part: { ...f.props.part, element: { current: f.node as HTMLElement | null } }, open: false };
  view.rerender(<Probe {...props} />); expect(records[0].disconnect).toHaveBeenCalledTimes(1); expect(f.latest()).toBe(close);
  props.frame.ref.current = null; view.rerender(<Probe {...props} />);
  expect(close.playState).toBe('idle'); close.finish(); await drain(); expect(props.onExited).not.toHaveBeenCalled();
  props.frame.ref.current = f.frame; view.rerender(<Probe {...props} />);
  expect(f.latest()).not.toBe(close);
  const replacement = document.createElement('div'); f.node.replaceWith(replacement); props.part.element.current = replacement;
  view.rerender(<Probe {...props} />); expect(records.at(-1)!.observe.mock.calls.map(call => call[0])).toEqual([replacement, f.frame]);
  const last = records.at(-1)!; a.resize(); const pending = [...a.queued.values()];
  view.rerender(<Probe {...props} present={false} />); expect(last.disconnect).toHaveBeenCalledTimes(1); expect(a.queued.size).toBe(0);
  const reads = f.readFrame.mock.calls.length; pending.forEach(callback => callback(0)); last.callback([], {} as ResizeObserver); a.flush();
  expect(f.readFrame).toHaveBeenCalledTimes(reads);
});

it.each([
  ['reader throws', false], ['foreign document', false], ['frame document', false], ['nonfinite geometry', false],
  ['reader throws', true], ['foreign document', true], ['frame document', true], ['nonfinite geometry', true],
] as const)('revokes playback before reporting update failure: %s, queued finish %s', async (kind, queuedFinish) => {
  const a = realm(window, document); observers(); const f = fixture(); const view = render(<Probe {...f.props} />);
  const close = startClose(f, view);
  if (queuedFinish) close.finish();
  const valid = f.readFrame()!;
  const error = kind === 'reader throws' ? new Error('reader failed') : new Error(kind === 'foreign document' || kind === 'frame document' ? 'Surface frames must share a document' : 'Unsupported surface frame');
  if (kind === 'frame document') { document.body.append(f.node); document.implementation.createHTMLDocument().body.append(f.frame); }
  f.set(kind === 'frame document' ? valid : kind === 'reader throws' ? error : kind === 'foreign document'
    ? { ...valid, document: document.implementation.createHTMLDocument() } : { ...valid, size: { ...valid.size, width: NaN } });
  a.resize(); expect(() => a.flush()).toThrow(error.message);
  expect(close.playState).toBe('idle');
  close.finish(); await drain(); expect(f.props.onExited).not.toHaveBeenCalled();
  f.set('live'); document.body.append(f.frame); f.frame.append(f.node);
  // A rejected binding must forget its old target: the same valid close can
  // settle immediately, rather than silently returning as already targeted.
  a.resize(); a.flush(); expect(f.props.onExited).toHaveBeenCalledTimes(1);
  view.rerender(<Probe {...f.props} />); a.resize(); a.flush(); expect(f.latest()).not.toBe(close);
  startClose(f, view).finish(); await drain(); expect(f.props.onExited).toHaveBeenCalledTimes(2);
});

it.each(['null', 'disconnected', 'paint null'] as const)('suspends %s without exit and recovers without stale finish', async kind => {
  const a = realm(window, document); observers(); const f = fixture(); const view = render(<Probe {...f.props} />); const close = startClose(f, view);
  if (kind === 'null') f.set(null);
  else if (kind === 'disconnected') f.frame.remove();
  else f.props.part.element.current = null;
  a.resize(); a.flush(); expect(close.playState).toBe('idle'); close.finish(); await drain(); expect(f.props.onExited).not.toHaveBeenCalled();
  f.set('live'); document.body.append(f.frame); f.frame.append(f.node); f.props.part.element.current = f.node;
  view.rerender(<Probe {...f.props} open={false} />); f.latest().finish(); await drain(); expect(f.props.onExited).toHaveBeenCalledTimes(1);
});

// Observe the rejected completion continuation without changing the production
// target or swallowing an unhandled rejection. Only promises registered during
// close acquisition are tracked; each rejection remains an explicit assertion.
function captureContinuations(run: () => void) {
  const original = Object.getOwnPropertyDescriptor(Promise.prototype, 'then')!.value as typeof Promise.prototype.then; const promises: Promise<unknown>[] = [];
  Promise.prototype.then = function (this: Promise<unknown>, yes, no) {
    const next = original.call(this, yes, no); promises.push(next); void original.call(next, undefined, () => {}); return next;
  } as typeof Promise.prototype.then;
  try { run(); } finally { Promise.prototype.then = original; }
  return promises;
}
it.each(['reader throws', 'foreign document', 'frame document', 'nonfinite geometry', 'null', 'disconnected', 'paint null', 'adopted'] as const)('rejects a close that becomes invalid before completion without observer delivery: %s', async kind => {
  realm(window, document); observers(); const f = fixture(); const view = render(<Probe {...f.props} />);
  const continuations = captureContinuations(() => { startClose(f, view); }); const close = f.latest();
  const valid = f.readFrame()!;
  if (kind === 'reader throws') f.set(new Error('reader failed'));
  else if (kind === 'foreign document') f.set({ ...valid, document: document.implementation.createHTMLDocument() });
  else if (kind === 'frame document') { document.body.append(f.node); document.implementation.createHTMLDocument().body.append(f.frame); f.set(valid); }
  else if (kind === 'adopted') { const b = secondRealm(); b.document.body.append(b.document.adoptNode(f.frame)); }
  else if (kind === 'nonfinite geometry') f.set({ ...valid, viewportFromLocal: { ...valid.viewportFromLocal, scaleX: Infinity } });
  else if (kind === 'null') f.set(null);
  else if (kind === 'disconnected') f.frame.remove();
  else f.props.part.element.current = null;
  close.finish(); const results = await Promise.allSettled(continuations); await drain();
  expect(f.props.onExited).not.toHaveBeenCalled();
  const errors = results.filter(result => result.status === 'rejected').map(result => result.reason as Error);
  expect(errors.map(error => error.message)).toEqual(kind === 'reader throws' ? ['reader failed']
    : kind === 'foreign document' || kind === 'frame document' ? ['Surface frames must share a document'] : kind === 'nonfinite geometry' ? ['Unsupported surface frame'] : []);
  f.set('live'); document.body.append(f.frame); f.frame.append(f.node); f.props.part.element.current = f.node;
  view.rerender(<Probe {...f.props} open={false} />); f.latest().finish(); await drain(); expect(f.props.onExited).toHaveBeenCalledTimes(1);
});
