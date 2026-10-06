import { act, render, cleanup, screen } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { useRef } from 'react';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Dialog } from '../dialog/public.tsx';
import { useState } from '../state/public.ts';
import { readMotionTransition } from './transition.ts';
import { springTrajectory } from './spring.ts';

function Scene() {
  const [open, setOpen] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  return <><button type="button" onClick={() => setOpen(true)}>Open dialog</button>
    <Dialog open={open} title="Motion dialog" initialFocusRef={input} onClose={() => setOpen(false)}>
      <label>Task name<input ref={input} defaultValue="Draft a note" /></label>
      <button type="button" onClick={() => setOpen(false)}>Done</button>
    </Dialog></>;
}
afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });

it('uses the shared physical spring without scaling text and preserves backdrop feedback', () => {
  render(<Scene />);
  act(() => { screen.getByRole('button', { name: 'Open dialog' }).click(); });
  const panel = screen.getByRole('dialog', { name: 'Motion dialog' });
  const animation = panel.getAnimations()[0];
  const effect = animation.effect as KeyframeEffect;
  expect(effect.getKeyframes().length).toBeGreaterThan(2);
  expect(effect.getTiming().duration).toBe(springTrajectory(0, 1, 0).duration);
  expect(effect.getTiming().easing).toBe('linear');
  const frames = panel.getAnimations().flatMap(animation => (animation.effect as KeyframeEffect).getKeyframes());
  expect(frames.some(frame => frame.scale !== undefined)).toBe(false);
  expect(frames.some(frame => frame.translate !== undefined)).toBe(true);
  const backdrop = panel.parentElement!;
  expect(backdrop.getAnimations().length).toBeGreaterThan(0);
  expect(parseFloat(getComputedStyle(backdrop).animationDuration)).toBe(readMotionTransition(backdrop, 'feedback').duration);
});

it('keeps focus and keyboard behavior immediate through rapid dismissal and reopening', async () => {
  render(<Scene />);
  const opener = screen.getByRole('button', { name: 'Open dialog' });
  await userEvent.click(opener);
  await expect.poll(() => document.activeElement).toBe(screen.getByRole('textbox', { name: 'Task name' }));
  await userEvent.keyboard('{Escape}');
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(document.activeElement).toBe(opener);
  await userEvent.click(opener);
  await userEvent.click(screen.getByRole('button', { name: 'Done' }));
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it('has no entrance travel under reduced motion and fits a compact viewport', async () => {
  await commands.emulateReducedMotion(true);
  await page.viewport(390, 844);
  render(<Scene />);
  await userEvent.click(screen.getByRole('button', { name: 'Open dialog' }));
  const panel = screen.getByRole('dialog', { name: 'Motion dialog' });
  expect(getComputedStyle(panel).animationName).toBe('none');
  expect(panel.getAnimations()).toHaveLength(0);
  expect(getComputedStyle(panel.parentElement!).animationName).toBe('none');
  expect(panel.getBoundingClientRect().left).toBeGreaterThanOrEqual(0);
  expect(panel.getBoundingClientRect().right).toBeLessThanOrEqual(390);
  await expect.poll(() => document.activeElement).toBe(screen.getByRole('textbox', { name: 'Task name' }));
});

it('discards an entry completion queued before dismissal without affecting the next dialog', async () => {
  render(<Scene />);
  act(() => { screen.getByRole('button', { name: 'Open dialog' }).click(); });
  const first = screen.getByRole('dialog', { name: 'Motion dialog' });
  for (const animation of first.getAnimations()) animation.finish();
  act(() => { screen.getByRole('button', { name: 'Done' }).click(); });
  expect(first.isConnected).toBe(false);
  expect(first.getAnimations()).toHaveLength(0);
  act(() => { screen.getByRole('button', { name: 'Open dialog' }).click(); });
  const second = screen.getByRole('dialog', { name: 'Motion dialog' });
  expect(second).not.toBe(first);
  await Promise.all(second.getAnimations().map(animation => animation.finished));
  expect(getComputedStyle(second).opacity).toBe('1');
  expect(screen.getByRole('textbox', { name: 'Task name' })).toBe(document.activeElement);
});
