import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import '../../../styles/entry.css';
import { readMotionTransition } from '../../../ui/motion/transition.ts';
import { ContextRing } from './context-ring.tsx';

afterEach(cleanup);

it('updates the semantic reading immediately while the arc consumes layout timing', () => {
  const usage = { used_tokens: 1000, context_window: 10000, percent: 10, at_ms: 0 };
  const { container, rerender } = render(<ContextRing usage={usage} />);
  const fill = container.querySelector('circle + circle')!;
  const motion = readMotionTransition(fill, 'layout');
  expect(getComputedStyle(fill).transitionDuration).toBe(`${motion.duration}s`);
  expect(getComputedStyle(fill).transitionTimingFunction).toBe(`cubic-bezier(${motion.ease.join(', ')})`);
  const before = fill.getAttribute('stroke-dasharray');
  rerender(<ContextRing usage={{ ...usage, used_tokens: 6000, percent: 60 }} />);
  expect(container.querySelector('[data-nc-context-ring]')?.getAttribute('aria-label')).toBe('6k of 10k in context');
  expect(fill.getAttribute('stroke-dasharray')).not.toBe(before);
  rerender(<ContextRing usage={{ ...usage, used_tokens: 12000, percent: null }} />);
  expect(container.querySelector('[data-nc-context-ring="over"]')).not.toBeNull();
  expect(container.querySelector('circle + circle')).toBeNull();
});

it('shows the new arc without travel under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  try {
    const { container } = render(<ContextRing usage={{ used_tokens: 1000, context_window: 10000, percent: 10, at_ms: 0 }} />);
    expect(getComputedStyle(container.querySelector('circle + circle')!).transitionProperty).toBe('none');
  } finally { await commands.emulateReducedMotion(false); }
});
