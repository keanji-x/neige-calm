import { act, render, cleanup, screen } from '@testing-library/react';
import { commands, page, userEvent } from 'vitest/browser';
import { useRef } from 'react';
import { afterEach, expect, it } from 'vitest';
import '../../styles/entry.css';
import { Dialog } from '../dialog/public.tsx';
import { useState } from '../state/public.ts';
import { readMotionTransition } from './transition.ts';

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

it('uses the shared entry recipe without scaling text and coordinates the backdrop', () => {
  render(<Scene />);
  act(() => { screen.getByRole('button', { name: 'Open dialog' }).click(); });
  const panel = screen.getByRole('dialog', { name: 'Motion dialog' });
  const style = getComputedStyle(panel);
  const recipe = readMotionTransition(panel, 'enter');
  expect(parseFloat(style.animationDuration)).toBe(recipe.duration);
  expect(style.animationTimingFunction).toBe(`cubic-bezier(${recipe.ease.join(', ')})`);
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
  expect(getComputedStyle(panel.parentElement!).animationName).toBe('none');
  expect(panel.getBoundingClientRect().left).toBeGreaterThanOrEqual(0);
  expect(panel.getBoundingClientRect().right).toBeLessThanOrEqual(390);
  await expect.poll(() => document.activeElement).toBe(screen.getByRole('textbox', { name: 'Task name' }));
});
