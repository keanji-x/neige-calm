/* Generic content exercises the primitive without chat policy or vendor components. */
import { cleanup, render, screen } from '@testing-library/react';
import { StrictMode } from 'react';
import { commands } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { SizeMotion } from './size.tsx';

function Panel({ mode, height = mode === 'expanded' ? 160 : 40, margin = 0 }: {
  mode: string; height?: number; margin?: number;
}) {
  return <SizeMotion motionKey={mode}>
    <div style={{ height, marginBlock: margin }}><button type="button">Keep focus</button></div>
  </SizeMotion>;
}
const host = () => document.querySelector<HTMLElement>('[data-nc-size-motion]')!;
const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
const settled = () => expect.poll(() => host().style.height).toBe('');
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

it('includes arbitrary content margins in its intrinsic size', () => {
  render(<Panel mode="compact" margin={16} />);
  expect(host().getBoundingClientRect().height).toBe(72);
  expect(host().firstElementChild!.getBoundingClientRect().height).toBe(72);
});

it('skips mount and ordinary changes, and animates only a mode change', async () => {
  const view = render(<Panel mode="compact" />);
  expect(host().style.height).toBe('');
  view.rerender(<Panel mode="compact" height={80} />);
  expect(host().style.height).toBe('');
  expect(host().getBoundingClientRect().height).toBe(80);
  view.rerender(<Panel mode="expanded" />);
  expect(host().style.height).not.toBe('');
  expect(host().getBoundingClientRect().height).toBe(80);
  await settled();
  expect(host().getBoundingClientRect().height).toBe(160);
  expect(getComputedStyle(host()).transform).toBe('none');
});

it('retargets intrinsic content while moving without remounting interactive children', async () => {
  const view = render(<Panel mode="compact" />);
  const button = screen.getByRole('button', { name: 'Keep focus' });
  button.focus();
  view.rerender(<Panel mode="expanded" />);
  await frame();
  view.rerender(<Panel mode="expanded" height={240} />);
  await settled();
  expect(host().getBoundingClientRect().height).toBe(240);
  expect(document.activeElement).toBe(button);
  expect(screen.getByRole('button', { name: 'Keep focus' })).toBe(button);
  expect(getComputedStyle(button).transform).toBe('none');
});

it('contains floated content and measures its real height', () => {
  render(<SizeMotion motionKey="panel"><div style={{ float: 'left', height: 80 }}>Floating content</div></SizeMotion>);
  expect(host().getBoundingClientRect().height).toBe(80);
});

it('releases active styles on unmount, including under StrictMode', async () => {
  const view = render(<StrictMode><Panel mode="compact" /></StrictMode>);
  view.rerender(<StrictMode><Panel mode="expanded" /></StrictMode>);
  const detached = host();
  expect(detached.style.height).not.toBe('');
  view.unmount();
  expect(detached.style.height).toBe('');
  expect(detached.style.overflow).toBe('');
  await frame();
  expect(detached.style.height).toBe('');
});

it('resolves directly to natural size under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const view = render(<Panel mode="compact" />);
  view.rerender(<Panel mode="expanded" margin={16} />);
  expect(host().style.height).toBe('');
  // The global reset leaves 0.01ms CSS transitions; wait for painting without a held JS height.
  await expect.poll(() => host().getBoundingClientRect().height).toBe(192);
  expect(host().style.height).toBe('');
});

it('lets the browser advance a large height change without per-frame inline rewrites', async () => {
  const view = render(<Panel mode="compact" />);
  view.rerender(<Panel mode="expanded" height={500} />);
  const animations = host().getAnimations();
  expect(animations).toHaveLength(1);
  const animation = animations[0];
  animation.pause();
  const effect = animation.effect as KeyframeEffect;
  expect(Number(effect.getTiming().duration)).toBeGreaterThan(240);
  expect(Number(effect.getTiming().duration)).toBeLessThanOrEqual(360);
  const initialInline = host().style.height;
  animation.currentTime = 100;
  await frame();
  expect(host().style.height).toBe(initialInline);
  expect(host().getBoundingClientRect().height).toBeGreaterThan(40);
  animation.finish();
  await settled();
  expect(host().getBoundingClientRect().height).toBe(500);
});

it('discards a native finish that is queued when the host unmounts', async () => {
  const view = render(<Panel mode="compact" />);
  view.rerender(<Panel mode="expanded" />);
  const detached = host();
  detached.getAnimations()[0].finish();
  view.unmount();
  await frame();
  expect(detached.style.height).toBe('');
  expect(detached.style.overflow).toBe('');
  expect(detached.getAnimations()).toHaveLength(0);
});

it('discards a queued finish before a newer mode owns the height', async () => {
  const view = render(<Panel mode="compact" />);
  view.rerender(<Panel mode="expanded" />);
  host().getAnimations()[0].finish();
  view.rerender(<Panel mode="compact" />);
  await frame();
  expect(host().getAnimations()).toHaveLength(1);
  await settled();
  expect(host().getBoundingClientRect().height).toBe(40);
});
