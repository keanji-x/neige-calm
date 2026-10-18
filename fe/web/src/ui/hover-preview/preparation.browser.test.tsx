import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { StrictMode, useEffect } from 'react';
import { commands, page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { HoverPreview } from './public.tsx';
import { readSurfaceFrame } from '../motion/geometry.ts';
import { placePreview } from './placement.ts';
vi.mock('./placement.ts', { spy: true });
vi.mock('../motion/geometry.ts', { spy: true });

afterEach(async () => { cleanup(); vi.mocked(readSurfaceFrame).mockReset(); vi.restoreAllMocks(); vi.unstubAllGlobals(); await commands.emulateReducedMotion(false); });

function observeLifetime() {
  const live = new Set<ResizeObserver>();
  const NativeResizeObserver = ResizeObserver;
  class TrackedResizeObserver extends NativeResizeObserver {
    override observe(target: Element, options?: ResizeObserverOptions) {
      live.add(this); super.observe(target, options);
    }
    override disconnect() {
      live.delete(this); super.disconnect();
    }
  }
  vi.stubGlobal('ResizeObserver', TrackedResizeObserver);
  const add = vi.spyOn(window, 'addEventListener'); const remove = vi.spyOn(window, 'removeEventListener');
  return () => {
    expect(live.size).toBe(0);
    const added = add.mock.calls.filter(([type]) => type === 'resize').map(([, listener]) => listener);
    const removed = remove.mock.calls.filter(([type]) => type === 'resize').map(([, listener]) => listener);
    expect(removed).toEqual(expect.arrayContaining(added));
    expect(removed).toHaveLength(added.length);
  };
}

for (const reduced of [false, true]) {
  it(`releases a first rejected long inline source and its resources before legal reopen (${reduced ? 'reduced' : 'normal'})`, async () => {
    await page.viewport(390, 844); await commands.emulateReducedMotion(reduced);
    const released = observeLifetime();
    const resources = new Set<object>(); const receive = vi.fn();
    function Resource() {
      useEffect(() => {
        const resource = {}; resources.add(resource);
        window.addEventListener('preview-resource', receive);
        const timer = setInterval(receive, 10000);
        return () => { resources.delete(resource); window.removeEventListener('preview-resource', receive); clearInterval(timer); };
      }, []);
      return <button>Resource content</button>;
    }
    function Fixture({ long }: { long: boolean }) {
      return <HoverPreview title="Impossible" trigger={activate => <a href="/preview-source" onClick={event => { event.preventDefault(); activate(); }}>
        Open {long ? 'source '.repeat(600) : 'source'}
      </a>}><Resource /></HoverPreview>;
    }
    const view = render(<StrictMode><Fixture long /></StrictMode>);
    const link = screen.getByRole('link');
    expect(link.parentElement!.getBoundingClientRect().height).toBeGreaterThan(844);
    await page.getByRole('link').click();
    expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
    expect(document.querySelector('[data-nc-preview-slot]')).toBeNull();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(resources.size).toBe(0); released();
    act(() => { window.dispatchEvent(new Event('preview-resource')); });
    expect(receive).not.toHaveBeenCalled();
    expect(document.getAnimations().length).toBe(0);
    view.rerender(<StrictMode><Fixture long={false} /></StrictMode>);
    act(() => { fireEvent.keyDown(screen.getByRole('link'), { key: 'ArrowDown' }); });
    const candidate = document.querySelector<HTMLElement>('[role=dialog]')!;
    expect(getComputedStyle(candidate).visibility).toBe('visible');
    expect(getComputedStyle(candidate.parentElement!).visibility).toBe('visible');
    const card = screen.getByRole('dialog');
    expect(resources.size).toBe(1);
    expect(card.inert).toBe(false);
    expect(document.activeElement).toBe(screen.getByRole('region'));
    expect(getComputedStyle(card).visibility).toBe('visible');
    const slot = card.closest('[data-nc-preview-slot]')!.getBoundingClientRect();
    expect(slot.left).toBeGreaterThanOrEqual(12); expect(slot.right).toBeLessThanOrEqual(378);
    expect(card.getAnimations().length).toBe(reduced ? 0 : 1);
    act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
    if (!reduced) {
      expect(card.isConnected).toBe(true); expect(card.inert).toBe(true);
      card.getAnimations().forEach(animation => animation.finish());
    }
    await expect.poll(() => document.querySelector('[data-nc-preview-slot]')).toBeNull();
    expect(resources.size).toBe(0); released();
  });
}

// Exercise a declared measurement port that is temporarily unavailable. No
// primitive DOM is rewritten; real trigger, styles, listeners and motion run.
for (const reduced of [false, true]) for (const reason of ['keyboard', 'outside', 'parent', 'same-commit', 'unmount'] as const) {
  it(`disposes preparation on ${reason} without awaiting a motion completion (${reduced ? 'reduced' : 'normal'})`, async () => {
    await page.viewport(1200, 900); await commands.emulateReducedMotion(reduced);
    const released = observeLifetime();
    const live = new Set<object>(); const calls = vi.fn();
    function Resource() {
      useEffect(() => {
        const token = {}; live.add(token);
        window.addEventListener('preparation-resource', calls);
        const timeout = setTimeout(calls, 10000);
        return () => { live.delete(token); window.removeEventListener('preparation-resource', calls); clearTimeout(timeout); };
      }, []);
      return <p>Pending child resource</p>;
    }
    const frame = vi.mocked(readSurfaceFrame);
    const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
    const view = render(<StrictMode><div data-testid="preparation-host" style={{ padding: 80 }}>
      <HoverPreview title="Owner" trigger={activate => <button onClick={activate}>Owner</button>}>
        <HoverPreview title="Pending" trigger={activate => <button onClick={activate}>Pending</button>}><Resource /></HoverPreview>
      </HoverPreview>
      <button>Outside</button>
    </div></StrictMode>);
    act(() => { fireEvent.click(screen.getByRole('button', { name: 'Owner' })); });
    const parent = screen.getByRole('dialog');
    parent.getAnimations().forEach(animation => { animation.pause(); animation.currentTime = 80; });
    // The child's declared frame is unavailable while its legal parent keeps
    // global Escape ownership. Preparation has no global layer identity.
    frame.mockImplementation(element => element?.querySelector(':scope > [aria-label="Preview: Pending"]') ? null : realFrame(element));
    const trigger = screen.getByRole('button', { name: 'Pending' });
    act(() => { fireEvent.click(trigger); });
    const pending = document.querySelector<HTMLElement>('[aria-label="Preview: Pending"]')!;
    expect(pending).not.toBeNull(); expect(live.size).toBe(1);
    expect(getComputedStyle(pending.parentElement!).clipPath).toBe('inset(50%)');
    expect(pending.getAnimations()).toHaveLength(0);
    if (reason === 'unmount') view.unmount();
    else act(() => {
      if (reason === 'keyboard') fireEvent.keyDown(document, { key: 'Escape' });
      if (reason === 'same-commit') fireEvent.keyDown(trigger, { key: 'Escape' });
      if (reason === 'outside') fireEvent.pointerDown(screen.getByRole('button', { name: 'Outside' }));
      if (reason === 'parent') fireEvent.scroll(screen.getByTestId('preparation-host'));
      if (reason === 'same-commit') fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    });
    if (reason === 'same-commit') {
      expect(pending.isConnected).toBe(true); expect(live.size).toBe(1);
      expect(getComputedStyle(pending.parentElement!).clipPath).toBe('inset(50%)');
      act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
    }
    expect(pending.isConnected).toBe(false); expect(live.size).toBe(0);
    act(() => { window.dispatchEvent(new Event('preparation-resource')); });
    expect(calls).not.toHaveBeenCalled();
    frame.mockImplementation(realFrame);
    if (reason !== 'unmount') {
      act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Owner' }), { key: 'ArrowDown' }); });
      act(() => { fireEvent.keyDown(screen.getByRole('button', { name: 'Pending' }), { key: 'ArrowDown' }); });
      const reopened = screen.getByRole('dialog', { name: 'Preview: Pending' });
      expect(reopened).not.toBe(pending); expect(reopened.getAnimations()).toHaveLength(reduced ? 0 : 1);
      expect(document.activeElement).toBe(screen.getByRole('region', { name: 'Preview content: Pending' }));
      view.unmount();
    }
    expect(live.size).toBe(0); released();
  });
}

it('reveals only a valid first frame then retains reversible paint until the current exit completes', async () => {
  await page.viewport(390, 844);
  const frame = vi.mocked(readSurfaceFrame); const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  frame.mockReturnValue(null);
  render(<StrictMode><div data-testid="preparation-host" style={{ padding: 80 }}><HoverPreview title="Preparing"
    trigger={activate => <button onClick={activate}>Preparing</button>}><p>Actual content</p></HoverPreview></div></StrictMode>);
  const trigger = screen.getByRole('button');
  act(() => { fireEvent.click(trigger); });
  const card = document.querySelector<HTMLElement>('[role="dialog"]')!;
  const slot = card.closest('[data-nc-preview-slot]')!;
  expect(getComputedStyle(card.parentElement!).clipPath).toBe('inset(50%)'); expect(card.getAnimations()).toHaveLength(0);
  await commands.emulateReducedMotion(true);
  expect(getComputedStyle(card.parentElement!).clipPath).toBe('inset(50%)');
  expect(card.getAnimations()).toHaveLength(0);
  await commands.emulateReducedMotion(false);
  frame.mockImplementation(realFrame);
  // An ordinary geometry input can finish preparation even when the slot and
  // content size are unchanged. No retry clock or unrelated render is needed.
  act(() => { window.dispatchEvent(new Event('resize')); });
  expect(screen.getByRole('dialog')).toBe(card);
  expect(card.getAnimations()).toHaveLength(1);
  const box = slot.getBoundingClientRect(); expect(box.left).toBeGreaterThanOrEqual(12); expect(box.right).toBeLessThanOrEqual(378);
  const entry = card.getAnimations()[0]; entry.pause(); entry.currentTime = 40;
  act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
  const oldExit = card.getAnimations()[0]; oldExit.pause(); oldExit.currentTime = 40;
  expect(card.isConnected).toBe(true); expect(card.inert).toBe(true);
  act(() => {
    // Queue the real native finished promise before reversal revokes its authority.
    oldExit.finish();
    fireEvent.keyDown(trigger, { key: 'ArrowDown' });
  });
  expect(screen.getByRole('dialog')).toBe(card); expect(card.inert).toBe(false);
  await Promise.resolve();
  expect(card.isConnected).toBe(true);
  await commands.emulateReducedMotion(true);
  await expect.poll(() => card.getAnimations().length).toBe(0);
  await commands.emulateReducedMotion(false);
  act(() => { fireEvent.keyDown(document, { key: 'Escape' }); });
  card.getAnimations().forEach(animation => animation.finish());
  await expect.poll(() => slot.isConnected).toBe(false);
});

it('finishes prepared paint after its source grows to leave no legal slot', async () => {
  await page.viewport(390, 844);
  function Fixture({ long }: { long: boolean }) {
    return <HoverPreview title="Growing" trigger={activate => <a href="/preview-source" onClick={event => { event.preventDefault(); activate(); }}>
      Open {long ? 'source '.repeat(600) : 'source'}
    </a>}><p>Content</p></HoverPreview>;
  }
  const view = render(<Fixture long={false} />);
  act(() => { fireEvent.click(screen.getByRole('link')); });
  const card = screen.getByRole('dialog');
  card.getAnimations()[0].pause(); card.getAnimations()[0].currentTime = 80;
  view.rerender(<Fixture long />);
  expect(card.inert).toBe(true); expect(card.isConnected).toBe(true);
  card.getAnimations().forEach(animation => animation.finish());
  await expect.poll(() => document.querySelector('[data-nc-preview-slot]')).toBeNull();
});


it('publishes first paint admission to later reservations even when its geometry is unchanged', async () => {
  await page.viewport(1200, 900);
  const frame = vi.mocked(readSurfaceFrame); const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  frame.mockImplementation(element => element?.querySelector(':scope > [aria-label="Preview: Admission first"]') ? null : realFrame(element));
  render(<div style={{ padding: 80 }}>
    {['Admission first', 'Admission second'].map(title => <HoverPreview key={title} title={title}
      trigger={activate => <button onClick={activate}>{title}</button>}><p>Content</p></HoverPreview>)}
  </div>);
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Admission first' })); });
  act(() => { fireEvent.click(screen.getByRole('button', { name: 'Admission second' })); });
  const first = document.querySelector<HTMLElement>('[aria-label="Preview: Admission first"]')!;
  const firstSlot = first.closest<HTMLElement>('[data-nc-preview-slot]')!;
  expect(firstSlot.inert).toBe(true);
  const second = screen.getByRole('dialog', { name: 'Preview: Admission second' });
  second.getAnimations().forEach(animation => animation.finish());
  // Drain initial observations before recovering an otherwise unchanged frame.
  await new Promise(requestAnimationFrame); await new Promise(requestAnimationFrame);
  vi.mocked(placePreview).mockClear(); frame.mockImplementation(realFrame);
  act(() => { window.dispatchEvent(new Event('resize')); });
  expect(firstSlot.inert).toBe(false);
  const secondSource = screen.getByRole('button', { name: 'Admission second' }).parentElement!.getBoundingClientRect();
  const input = vi.mocked(placePreview).mock.calls.filter(([value]) => value.anchor.left === secondSource.left).at(-1)![0];
  expect(input.obstacles).toEqual([firstSlot.getBoundingClientRect()]);
});


for (const reduced of [false, true]) it(`revokes explicit preparation on host inert and rejects later geometry readiness (${reduced ? 'reduced' : 'normal'})`, async () => {
  await page.viewport(1200, 900); await commands.emulateReducedMotion(reduced);
  const { readSurfaceFrame: realFrame } = await vi.importActual<typeof import('../motion/geometry.ts')>('../motion/geometry.ts');
  vi.mocked(readSurfaceFrame).mockReturnValue(null);
  render(<section style={{ padding: 80 }}><HoverPreview title="Host preparation"
    trigger={activate => <button onClick={activate}>Host preparation</button>}>Content</HoverPreview></section>);
  const trigger = page.getByRole('button', { name: 'Host preparation' });
  await trigger.click();
  const card = document.querySelector<HTMLElement>('[aria-label="Preview: Host preparation"]')!;
  expect(card.inert).toBe(true); expect(card.isConnected).toBe(true);
  const host = trigger.element().closest('section')!;
  try {
    await act(async () => { host.inert = true; await Promise.resolve(); });
    expect(card.isConnected).toBe(false);
    host.inert = false; vi.mocked(readSurfaceFrame).mockImplementation(realFrame);
    act(() => { window.dispatchEvent(new Event('resize')); });
    await new Promise(resolve => setTimeout(resolve, 400));
    expect(document.querySelector('[data-nc-link-preview]')).toBeNull();
    await trigger.click();
    await expect.element(page.getByRole('dialog', { name: 'Preview: Host preparation' })).toBeVisible();
  } finally { host.inert = false; await trigger.unhover(); }
});
