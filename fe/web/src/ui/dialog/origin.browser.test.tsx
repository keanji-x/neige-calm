import { act, cleanup, render, screen } from '@testing-library/react';
import { useRef } from 'react';
import { afterEach, expect, it } from 'vitest';
import { commands, page, userEvent } from 'vitest/browser';
import '../../styles/entry.css';
import { Dialog, useDialogView, type DialogOrigin } from './public.tsx';
import { DepthFeedback } from '../motion/depth-feedback.tsx';
import { useState } from '../state/public.ts';

const tick = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

function Child() {
  const view = useDialogView();
  return <button onClick={() => view?.pushView({ title: 'Child', body: <button>Child action</button> })}>Browse child</button>;
}
function Scene({ firstNull = false, center = false, removeOnOpen = false }: { firstNull?: boolean; center?: boolean; removeOnOpen?: boolean }) {
  const ref = useRef<HTMLSpanElement | null>(null);
  const other = useRef<HTMLSpanElement | null>(null);
  const input = useRef<HTMLInputElement | null>(null);
  const [acquired, setAcquired] = useState(false);
  const [open, setOpen] = useState(false);
  const [replace, setReplace] = useState(false);
  const [switchOrigin, setSwitchOrigin] = useState(false);
  const [wide, setWide] = useState(false);
  const source = firstNull ? other : ref;
  const origin: DialogOrigin = center || switchOrigin ? { kind: 'surface-center' }
    : { kind: 'anchor', anchor: { ref: source }, unavailable: 'surface-center' };
  return <><DepthFeedback />{!(removeOnOpen && open) && <span key={replace ? 'replacement' : 'original'} ref={ref} data-dialog-source=""
    style={{ position: 'fixed', left: replace ? 160 : 24, top: 60, display: 'inline-flex' }}>
    <button onClick={() => setOpen(true)}>Open source</button></span>}
    {acquired && <span ref={other} style={{ position: 'fixed', left: 300, top: 40 }}>Late source</span>}
    <Dialog purpose="info" initialFocusRef={input} open={open} origin={origin} title="Origin" wide={wide} onClose={() => setOpen(false)}>
      <input ref={input} aria-label="Draft" defaultValue="Keep selection" /><Child /><button onClick={() => setAcquired(true)}>Acquire source</button>
      <button onClick={() => setReplace(true)}>Replace source</button>
      <button onClick={() => setSwitchOrigin(true)}>Change declaration</button>
      <button onClick={() => setWide(true)}>Widen</button>
      <button onClick={() => setOpen(false)}>Done</button>
    </Dialog></>;
}
function panel() { return screen.getByRole('dialog', { name: 'Origin' }); }
async function hold(node = panel()) {
  const animation = node.getAnimations()[0];
  expect(animation).toBeDefined();
  await animation.ready;
  animation.pause(); animation.currentTime = 0;
  await tick();
  return animation;
}
function fixedPoint(node: HTMLElement, point: { x: number; y: number }) {
  const frame = node.parentElement!.getBoundingClientRect();
  const matrix = new DOMMatrix(getComputedStyle(node).transform);
  const projected = new DOMPoint(point.x - frame.left, point.y - frame.top).matrixTransform(matrix);
  expect(frame.left + projected.x).toBeCloseTo(point.x, 3);
  expect(frame.top + projected.y).toBeCloseTo(point.y + .56, 3);
}
it('uses the stable explicit anchor independently of pressed button and focus', async () => {
  await page.viewport(1200, 800);
  render(<Scene />);
  await commands.mouseButton('[data-dialog-source] button', true);
  const source = document.querySelector<HTMLElement>('[data-dialog-source]')!;
  expect(source.getAnimations()).toHaveLength(0);
  expect(screen.getByRole('button', { name: 'Open source' }).getAnimations()).toHaveLength(1);
  await commands.mouseButton('[data-dialog-source] button', false);
  const animation = await hold();
  const rect = source.getBoundingClientRect();
  fixedPoint(panel(), { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
  await expect.element(page.getByRole('textbox', { name: 'Draft' })).toHaveFocus();
  animation.play();
  await userEvent.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(animation.playState).toBe('idle');
});
it.each(['center', 'first-null', 'same-commit-unmount'] as const)('uses a whole-cycle surface center for %s', async scenario => {
  render(<Scene center={scenario === 'center'} firstNull={scenario === 'first-null'} removeOnOpen={scenario === 'same-commit-unmount'} />);
  await page.getByRole('button', { name: 'Open source' }).click();
  if (scenario === 'same-commit-unmount') expect(document.querySelector('[data-dialog-source]')).toBeNull();
  const animation = await hold();
  act(() => screen.getByRole('button', { name: 'Acquire source' }).click());
  expect(panel().getAnimations()[0]).toBe(animation);
  const rect = panel().parentElement!.getBoundingClientRect();
  fixedPoint(panel(), { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 });
});
it('locks the kind/ref declaration and freezes replacement without restarting child lift', async () => {
  render(<Scene />);
  await page.getByRole('button', { name: 'Open source' }).click();
  const animation = await hold();
  const before = getComputedStyle(panel()).transform;
  act(() => screen.getByRole('button', { name: 'Change declaration' }).click());
  expect(panel().getAnimations()[0]).toBe(animation);
  act(() => screen.getByRole('button', { name: 'Replace source' }).click());
  await tick();
  expect(getComputedStyle(panel()).transform).toBe(before);
  expect(panel().getAnimations()[0]).toBe(animation);
  animation.finish(); await animation.finished;
  await page.getByRole('button', { name: 'Browse child' }).click();
  await expect.element(page.getByRole('button', { name: 'Child action' })).toHaveFocus();
  expect(screen.getByRole('dialog', { name: 'Child' })).toBe(panelElement(animation));
  await userEvent.keyboard('{Escape}');
  await expect.element(page.getByRole('button', { name: 'Browse child' })).toHaveFocus();
  expect(screen.getByRole('textbox', { name: 'Draft' })).toHaveValue('Keep selection');
});
function panelElement(animation: Animation) { return (animation.effect as KeyframeEffect).target; }
it('rebases wide layout and cancels immediately under reduced motion and close', async () => {
  render(<Scene />);
  await page.getByRole('button', { name: 'Open source' }).click();
  const first = await hold();
  first.currentTime = 40; await tick();
  const node = panel(); const before = new DOMMatrix(getComputedStyle(node).transform);
  act(() => screen.getByRole('button', { name: 'Widen' }).click());
  const next = node.getAnimations()[0]; await next.ready; next.pause(); next.currentTime = 0; await tick();
  const after = new DOMMatrix(getComputedStyle(node).transform);
  expect(after.e).toBeCloseTo(before.e, 3);
  expect(after.f).toBeCloseTo(before.f, 3);
  expect(after.a).toBeCloseTo(before.a, 3);
  expect(first.playState).toBe('idle');
  await commands.emulateReducedMotion(true);
  await expect.poll(() => node.getAnimations().length).toBe(0);
  await page.getByRole('button', { name: 'Done' }).click();
  expect(node.isConnected).toBe(false);
});

function ScrollScene() {
  const ref = useRef<HTMLSpanElement | null>(null);
  const [open, setOpen] = useState(false);
  return <><div data-testid="source-scrollport" style={{ position: 'fixed', left: 20, top: 20, width: 240, height: 80, overflow: 'auto' }}>
    <div style={{ height: 400, paddingTop: 20 }}><span ref={ref} style={{ display: 'inline-flex' }}>
      <button onClick={() => setOpen(true)}>Open scrolling source</button>
    </span></div>
  </div><Dialog purpose="info" open={open} origin={{ kind: 'anchor', anchor: { ref }, unavailable: 'surface-center' }} title="Origin" onClose={() => setOpen(false)}>
    <button>Ready</button>
  </Dialog></>;
}
it('rebases actual source scrolling and viewport resize on the same Motion contribution', async () => {
  await page.viewport(1200, 800);
  render(<ScrollScene />);
  await page.getByRole('button', { name: 'Open scrolling source' }).click();
  const first = await hold();
  const before = new DOMMatrix(getComputedStyle(panel()).transform);
  const scrollport = screen.getByTestId('source-scrollport');
  const sourceBefore = scrollport.querySelector('span')!.getBoundingClientRect().top;
  scrollport.scrollTop = 8;
  await expect.poll(() => panel().getAnimations()[0]).not.toBe(first);
  const scrolled = await hold();
  expect(scrollport.querySelector('span')!.getBoundingClientRect().top).toBe(sourceBefore - 8);
  const afterScroll = new DOMMatrix(getComputedStyle(panel()).transform);
  expect(afterScroll.e).toBeCloseTo(before.e, 3);
  expect(afterScroll.f).toBeCloseTo(before.f, 3);
  const height = window.visualViewport!.height;
  await page.viewport(900, 600);
  await expect.poll(() => panel().getAnimations()[0]).not.toBe(scrolled);
  const resized = await hold();
  const afterResize = new DOMMatrix(getComputedStyle(panel()).transform);
  expect(afterResize.e).toBeCloseTo(afterScroll.e, 3);
  expect(afterResize.f).toBeCloseTo(afterScroll.f, 3);
  expect(window.visualViewport!.height).toBeLessThan(height);
  resized.play();
  await expect.poll(() => panel().getAnimations().length).toBe(0);
});
