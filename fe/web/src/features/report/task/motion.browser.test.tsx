import { cleanup, render } from '@testing-library/react';
import { commands, userEvent } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';
import '../../../styles/entry.css';
import { ReportTaskBlock } from './public.tsx';
import { readMotionTransition } from '../../../ui/motion/transition.ts';

afterEach(async () => { cleanup(); await commands.emulateReducedMotion(false); });
function task() {
  render(<ReportTaskBlock blockId="motion-task" payload={{ key: 'Review motion', kind: 'codex', declared_by: 'user', ready: true, goal: 'Keep disclosure responsive.' }} />);
  const summary = document.querySelector<HTMLElement>('[data-nc-task-state] > summary')!;
  return { summary, marker: summary.firstElementChild! };
}
it('uses the shared disclosure recipe on the real task and preserves native expansion', async () => {
  const { summary, marker } = task();
  const motion = readMotionTransition(marker, 'disclosure');
  expect(parseFloat(getComputedStyle(marker).transitionDuration)).toBe(motion.duration);
  expect(getComputedStyle(marker).transitionTimingFunction).toBe(`cubic-bezier(${motion.ease.join(', ')})`);
  await userEvent.click(summary);
  expect(summary.closest('details')!.open).toBe(true);
  await expect.poll(() => getComputedStyle(marker).rotate).toBe('90deg');
  expect(getComputedStyle(marker).scale).toBe('none');
});
it('expands without marker travel under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  const { summary, marker } = task();
  await userEvent.click(summary);
  expect(summary.closest('details')!.open).toBe(true);
  expect(getComputedStyle(marker).transitionProperty).toBe('none');
  expect(marker.getAnimations()).toHaveLength(0);
});
