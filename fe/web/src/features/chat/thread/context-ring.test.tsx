// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import { ContextRing, contextRingState, formatTokens } from './context-ring.tsx';
import type { PlannerRunTokenUsage } from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

function usage(over: Partial<PlannerRunTokenUsage> = {}): PlannerRunTokenUsage {
  return {
    used_tokens: 24_100, context_window: 258_400, percent: 4.9,
    at_ms: 1_700_000_000_000, ...over,
  };
}

function ring(): HTMLElement | null {
  return document.querySelector('[data-nc-context-ring]');
}

/** The drawn arc, as a fraction of the circle. */
function drawnFraction(): number {
  const fill = document.querySelector('[data-nc-context-ring] circle + circle');
  /* A round line cap paints a dot at a dash length of zero, so "no arc" has to mean no element. */
  if (fill === null) return 0;
  const dash = fill.getAttribute('stroke-dasharray') ?? '0';
  const radius = Number(fill.getAttribute('r'));
  return Number(dash.split(' ')[0]) / (2 * Math.PI * radius);
}

describe('context ring', () => {
  it('draws the arc from the percent the server computed, never from the two counts', () => {
    /* 24.1k / 258.4k is 9.3%; the kernel says 4.9% because it takes the prompt-and-tools floor off both sides. */
    render(<ContextRing usage={usage()} />);
    expect(ring()?.getAttribute('data-nc-context-ring')).toBe('5');
    expect(drawnFraction()).toBeCloseTo(0.049, 3);
    expect(ring()?.getAttribute('aria-label')).toBe('24.1k of 258k in context');
  });

  it('says the counts on hover, in the words a person asked for them in', async () => {
    render(<ContextRing usage={usage()} />);
    await userEvent.hover(screen.getByRole('img', { name: /in context/ }));
    expect(await screen.findByText('24.1k of 258k in context')).toBeTruthy();
  });

  it('draws nothing when no window has been reported', () => {
    render(<ContextRing usage={usage({ context_window: null, percent: null })} />);
    expect(ring()).toBeNull();
  });

  it('draws nothing when the window is at or below the floor', () => {
    render(<ContextRing usage={usage({ context_window: 8_000, used_tokens: 4_000, percent: null })} />);
    expect(ring()).toBeNull();
  });

  it('says the readout in the label too, so it is not hover-only', () => {
    /* No tab stop on a readout, so the tooltip is unreachable without a
       mouse. The label is what makes that acceptable rather than a hole. */
    render(<ContextRing usage={usage()} />);
    expect(ring()?.hasAttribute('tabindex')).toBe(false);
    expect(ring()?.getAttribute('aria-label')).toBe('24.1k of 258k in context');
  });

  it('shows a count that overshot its window as its own state, not as a full ring', async () => {
    render(<ContextRing usage={usage({ used_tokens: 2_361_529, context_window: 258_400, percent: null })} />);
    expect(ring()?.getAttribute('data-nc-context-ring')).toBe('over');
    /* No arc: the kernel withholds a percentage here rather than clamping, and a full ring would undo that. */
    expect(drawnFraction()).toBe(0);
    /* And no circle drawn at all — a zero-length round cap is a dot. */
    expect(document.querySelectorAll('[data-nc-context-ring] circle')).toHaveLength(1);
    await userEvent.hover(screen.getByRole('img', { name: /more than the window holds/ }));
    expect(await screen.findByText(/more than the window holds/)).toBeTruthy();
  });

  it('draws nothing at all before the harness has reported anything', () => {
    render(<ContextRing usage={null} />);
    expect(ring()).toBeNull();
  });

  it('classifies without inventing a state for a reading it was not given', () => {
    expect(contextRingState(null)).toEqual({ kind: 'none' });
    expect(contextRingState(usage()).kind).toBe('filled');
  });

  it('formats counts the way a window is written down', () => {
    expect(formatTokens(980)).toBe('980');
    expect(formatTokens(24_100)).toBe('24.1k');
    expect(formatTokens(258_400)).toBe('258k');
  });
});
