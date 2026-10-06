import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { commands } from 'vitest/browser';

import '../../../styles/entry.css';
import { ContextRing } from './context-ring.tsx';

afterEach(cleanup);

it('updates the semantic reading immediately while the arc consumes the shared spring', () => {
  const usage = { used_tokens: 1000, context_window: 10000, percent: 10, at_ms: 0 };
  const { container, rerender } = render(<ContextRing usage={usage} />);
  const fill = container.querySelector('circle + circle')!;
  expect(fill.getAnimations()).toHaveLength(0);
  expect(getComputedStyle(fill).transitionProperty).toBe('none');
  const before = fill.getAttribute('stroke-dasharray');
  rerender(<ContextRing usage={{ ...usage, used_tokens: 6000, percent: 60 }} />);
  expect(container.querySelector('[data-nc-context-ring]')?.getAttribute('aria-label')).toBe('6k of 10k in context');
  expect(fill.getAttribute('stroke-dasharray')).not.toBe(before);
  expect(fill.getAnimations()).toHaveLength(1);
  expect((fill.getAnimations()[0].effect as KeyframeEffect).getKeyframes().length).toBeGreaterThan(2);
  rerender(<ContextRing usage={{ ...usage, used_tokens: 12000, percent: null }} />);
  expect(container.querySelector('[data-nc-context-ring="over"]')).not.toBeNull();
  expect(container.querySelector('circle + circle')).toBeNull();
});

it('shows the new arc without travel under reduced motion', async () => {
  await commands.emulateReducedMotion(true);
  try {
    const { container, rerender } = render(<ContextRing usage={{ used_tokens: 1000, context_window: 10000, percent: 10, at_ms: 0 }} />);
    rerender(<ContextRing usage={{ used_tokens: 6000, context_window: 10000, percent: 60, at_ms: 0 }} />);
    const fill = container.querySelector('circle + circle')!;
    expect(getComputedStyle(fill).transitionProperty).toBe('none');
    expect(fill.getAnimations()).toHaveLength(0);
  } finally { await commands.emulateReducedMotion(false); }
});
