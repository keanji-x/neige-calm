import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands, page } from 'vitest/browser';
import '../../styles/entry.css';
import { Drawer } from './public.tsx';

function Scene({ open }: { open: boolean }) {
  return <div style={{ position: 'relative', height: 500, width: 700 }}>
    <Drawer open={open} title="Continuity" onClose={() => {}}><input aria-label="Live draft" defaultValue="Keep this" /></Drawer>
  </div>;
}
const panel = () => screen.getByRole('complementary', { name: 'Continuity' });
const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
function pauseAt(element: Element, fraction: number) {
  const animations = element.getAnimations();
  expect(animations.length).toBeGreaterThan(0);
  for (const animation of animations) {
    animation.pause();
    animation.currentTime = Number(animation.effect!.getTiming().duration) * fraction;
  }
}
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); await page.viewport(1280, 720); });

it('reverses entrance and exit from the painted opacity while preserving the live draft', async () => {
  await page.viewport(1280, 720);
  const view = render(<Scene open={true} />);
  pauseAt(panel(), .3);
  const beforeClose = Number(getComputedStyle(panel()).opacity);
  expect(beforeClose).toBeGreaterThan(0);
  expect(beforeClose).toBeLessThan(1);
  const draft = screen.getByRole('textbox', { name: 'Live draft' });
  view.rerender(<Scene open={false} />);
  expect(Number(getComputedStyle(panel()).opacity)).toBeCloseTo(beforeClose, 2);
  pauseAt(panel(), .4);
  const beforeReopen = Number(getComputedStyle(panel()).opacity);
  view.rerender(<Scene open={true} />);
  expect(Number(getComputedStyle(panel()).opacity)).toBeCloseTo(beforeReopen, 2);
  expect(screen.getByRole('textbox', { name: 'Live draft' })).toBe(draft);
  await Promise.all(panel().getAnimations().map(animation => animation.finished));
  expect(getComputedStyle(panel()).opacity).toBe('1');
});

it('dismisses without waiting when reduced motion changes during an exit', async () => {
  await page.viewport(1280, 720);
  const view = render(<Scene open={true} />);
  await Promise.all(panel().getAnimations().map(animation => animation.finished));
  view.rerender(<Scene open={false} />);
  const departing = panel();
  pauseAt(departing, .3);
  await commands.emulateReducedMotion(true);
  await frame();
  await expect.poll(() => departing.isConnected).toBe(false);
});

it('does not wait for a nonexistent exit when closed before first paint', async () => {
  const view = render(<Scene open={true} />);
  view.rerender(<Scene open={false} />);
  await expect.poll(() => screen.queryByRole('complementary', { name: 'Continuity' })).toBeNull();
});

it('ignores a child transition ending while its own exit is in flight', async () => {
  const view = render(<Scene open={true} />);
  await Promise.all(panel().getAnimations().map(animation => animation.finished));
  view.rerender(<Scene open={false} />);
  const departing = panel();
  pauseAt(departing, .3);
  screen.getByRole('textbox', { name: 'Live draft' }).dispatchEvent(new TransitionEvent('transitionend', {
    propertyName: 'opacity', bubbles: true,
  }));
  expect(departing.isConnected).toBe(true);
  for (const animation of departing.getAnimations()) animation.finish();
  await expect.poll(() => departing.isConnected).toBe(false);
});

it('ignores an entrance completion queued before a new exit owns the panel', async () => {
  const view = render(<Scene open={true} />);
  const departing = panel();
  for (const animation of departing.getAnimations()) animation.finish();
  view.rerender(<Scene open={false} />);
  await frame();
  expect(departing.isConnected).toBe(true);
  expect(Number(getComputedStyle(departing).opacity)).toBeGreaterThan(0);
  await expect.poll(() => departing.isConnected).toBe(false);
});
