import { act, cleanup, render } from '@testing-library/react';
import { StrictMode } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands } from 'vitest/browser';
import { useSurfacePresence, type SurfacePresenceInput } from './surface-presence.ts';
import { readSurfaceFrame, type BindingView, type SurfaceBinding } from './geometry.ts';

afterEach(async () => { cleanup(); document.querySelectorAll('[data-measurement-test]').forEach(node => node.remove()); vi.unstubAllGlobals(); await commands.emulateReducedMotion(false); });
const frames = async (count: number) => { for (let i = 0; i < count; i++) await new Promise(requestAnimationFrame); };
function part(left: number) {
  const actual = document.createElement('div'); actual.dataset.measurementTest = '';
  actual.style.cssText = `position:fixed;left:${left}px;top:60px;width:120.5px;height:90.25px`;
  const node = actual.appendChild(document.createElement('div')); node.style.cssText = 'width:100%;height:100%';
  document.body.append(actual);
  return { actual, node, element: { current: node as HTMLElement | null }, readFrame: () => readSurfaceFrame(actual) };
}
for (const reduced of [false, true]) {
  it(`reports committed measurement edges without paint or same-value notices (${reduced ? 'reduced' : 'normal'})`, async () => {
    await commands.emulateReducedMotion(reduced);
    const primary = part(230); let available = true; let port: SurfaceBinding;
    const notices: BindingView[] = []; const exited = vi.fn();
    const input: SurfacePresenceInput = { parts: { kind: 'single', primary: { ...primary, readFrame: () => available ? primary.readFrame() : null } },
      geometry: { kind: 'lift', origin: { kind: 'surface-center' } }, open: true, present: true, enabled: true, onExited: exited,
      onMeasurementChange: view => {
        notices.push(view); expect(port.readBinding()).toBe(view);
        if (view.kind === 'ready') expect(view.parts.map(part => part.element)).toEqual([primary.node]);
      } };
    function Probe(props: SurfacePresenceInput) { port = useSurfacePresence(props); return null; }
    const view = render(<Probe {...input} />);
    await frames(4); expect(notices.map(view => view.kind)).toEqual(['ready']);
    const first = notices[0];
    available = false;
    act(() => { document.dispatchEvent(new Event('scroll')); }); await frames(3);
    expect(notices.map(view => view.kind)).toEqual(['ready', 'unavailable']);
    expect(exited).not.toHaveBeenCalled();
    available = true;
    act(() => { document.dispatchEvent(new Event('scroll')); }); await frames(3);
    expect(notices.map(view => view.kind)).toEqual(['ready', 'unavailable', 'ready']);
    expect(notices[2].lifetime).toBe(first.lifetime); expect(notices[2].revision).toBe(first.revision + 2);
    const restored = notices[2]; view.rerender(<Probe {...input} />);
    await frames(20); expect(notices).toHaveLength(3); expect(port!.readBinding()).toBe(restored);
    view.rerender(<Probe {...input} open={false} />);
    primary.node.getAnimations().forEach(animation => animation.finish());
    await expect.poll(() => exited.mock.calls.length).toBe(1);
    view.unmount(); expect(port!.readBinding().kind).toBe('unavailable');
    const count = notices.length; await frames(3); expect(notices).toHaveLength(count);
  });
}

it('invalidates replay lifetimes and paired replacement tokens before notifying', async () => {
  const primary = part(200); const peer = part(350); const replacement = part(500);
  let port: SurfaceBinding; const notices: BindingView[] = [];
  const onMeasurementChange = (view: BindingView) => { notices.push(view); expect(port.readBinding()).toBe(view); };
  const input: SurfacePresenceInput = { parts: { kind: 'paired', primary, peer }, geometry: { kind: 'lift', origin: { kind: 'surface-center' } },
    open: true, present: true, enabled: true, onExited: vi.fn(), onMeasurementChange };
  function Probe(props: SurfacePresenceInput) { port = useSurfacePresence(props); return null; }
  const view = render(<StrictMode><Probe {...input} /></StrictMode>);
  expect(notices).toHaveLength(2); expect(notices[0].lifetime).not.toBe(notices[1].lifetime);
  const old = port!.readBinding();
  peer.element.current = replacement.node;
  expect(port!.readBinding().kind).toBe('unavailable');
  view.rerender(<StrictMode><Probe {...input} /></StrictMode>);
  const current = port!.readBinding(); expect(current).not.toBe(old); expect(current.kind).toBe('ready');
  if (current.kind === 'ready') expect(current.parts.map(part => part.element)).toEqual([primary.node, replacement.node]);
  peer.element.current = null;
  act(() => { window.dispatchEvent(new Event('resize')); }); await frames(3);
  expect(port!.readBinding().kind).toBe('unavailable');
  await commands.emulateReducedMotion(true);
  expect(input.onExited).not.toHaveBeenCalled();
  peer.element.current = replacement.node;
  act(() => { window.dispatchEvent(new Event('resize')); }); await frames(3);
  expect(port!.readBinding().kind).toBe('ready');
  await commands.emulateReducedMotion(false); await frames(3);
  expect(primary.node.getAnimations()).toHaveLength(0);
  view.unmount(); expect(port!.readBinding().kind).toBe('unavailable');
});

it('rejects a captured observer delivery after its measurement lifetime is released', async () => {
  const NativeObserver = ResizeObserver;
  const deliveries: { observer: ResizeObserver; notify: ResizeObserverCallback }[] = [];
  class Observer extends NativeObserver {
    constructor(notify: ResizeObserverCallback) { super(notify); deliveries.push({ observer: this, notify }); }
  }
  vi.stubGlobal('ResizeObserver', Observer);
  const primary = part(200); const notices = vi.fn(); let port: SurfaceBinding;
  function Probe() {
    port = useSurfacePresence({ parts: { kind: 'single', primary }, geometry: { kind: 'lift', origin: { kind: 'surface-center' } },
      open: true, present: true, enabled: true, onExited: vi.fn(), onMeasurementChange: notices });
    return null;
  }
  const view = render(<Probe />); await frames(3);
  const ready = port!.readBinding(); const count = notices.mock.calls.length;
  view.unmount(); expect(primary.node.isConnected).toBe(true);
  // Keep real native observer registration/delivery and inject only a captured
  // late callback after disconnect, rather than simulating geometry or physics.
  act(() => { for (const delivery of deliveries) delivery.notify([], delivery.observer); });
  await frames(3);
  expect(notices).toHaveBeenCalledTimes(count);
  expect(primary.node.getAnimations()).toHaveLength(0);
  expect(port!.readBinding().kind).toBe('unavailable');
  expect(port!.readBinding().lifetime).not.toBe(ready.lifetime);
});
