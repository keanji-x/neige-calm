import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import '../../../styles/entry.css';
import { readMotionTransition } from '../../../ui/motion/transition.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

function mount() {
  const { container } = render(<ReportDocument report={{ summary: '', body: '', blocks: [
    { id: 'arrival', kind: 'prose', payload: { markdown: '# Arrival\n\nA highlighted paragraph.' } },
  ] }} empty={null} />);
  const target = container.querySelector('h2')!;
  target.setAttribute('data-nc-arrived', '');
  return target;
}

it('emphasizes an existing report heading with one shared background animation', async () => {
  const target = mount();
  const style = getComputedStyle(target);
  const motion = readMotionTransition(target, 'emphasis');
  expect(style.animationDuration).toBe(`${motion.duration}s`);
  expect(style.animationTimingFunction).toBe(`cubic-bezier(${motion.ease.join(', ')})`);
  expect(style.animationIterationCount).toBe('1');
  const animation = target.getAnimations()[0];
  expect(animation).toBeDefined();
  expect((animation.effect as KeyframeEffect).getKeyframes().every(frame => !('transform' in frame))).toBe(true);
  await animation.finished;
  await waitFor(() => expect(getComputedStyle(target).backgroundColor).toBe('rgba(0, 0, 0, 0)'));
});

it('keeps the report heading in place without highlighting under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  try { expect(getComputedStyle(mount()).animationName).toBe('none'); }
  finally { await commands.emulateReducedMotion(false); }
});
