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

describe('context ring', () => {
  it('draws the arc from the percent the server computed, never from the two counts', () => {
    /*
     * The whole reason this component exists as a component. 24.1k / 258.4k is
     * 9.3%; the kernel says 4.9%, because it takes the prompt-and-tools floor
     * off both sides. A ring drawn from the counts would be nearly twice as
     * full as the truth, and would look completely plausible.
     */
    render(<ContextRing usage={usage()} />);
    expect(ring()?.getAttribute('data-nc-context-ring')).toBe('5');
    /* The label carries the whole readout, because there is no tab stop here
       to reach the tooltip with — see the note on the trigger. */
    expect(ring()?.getAttribute('aria-label'))
      .toBe('24.1k of 258k in context, 5% of what this thread can use');
  });

  it('says the counts on hover, in the words a person asked for them in', async () => {
    render(<ContextRing usage={usage()} />);
    await userEvent.hover(screen.getByRole('img', { name: /in context/ }));
    expect(await screen.findByText('24.1k of 258k in context')).toBeTruthy();
  });

  /*
   * `percent === null` has three causes and they do not deserve one rendering.
   * Two are "nothing to measure against"; the third is a measured anomaly the
   * kernel deliberately refuses to clamp into a plausible full bar, and
   * flattening it back into one here would undo that decision one layer up.
   */
  it('draws nothing when no window has been reported', () => {
    render(<ContextRing usage={usage({ context_window: null, percent: null })} />);
    expect(ring()).toBeNull();
  });

  it('draws nothing when the window is at or below the floor', () => {
    /* The kernel withheld the percentage and the count did NOT overshoot, so
       from out here this is the same "nothing to say" as the case above — and
       reaching that conclusion took no knowledge of what the floor is. */
    render(<ContextRing usage={usage({ context_window: 8_000, used_tokens: 4_000, percent: null })} />);
    expect(ring()).toBeNull();
  });

  it('says the readout in the label too, so it is not hover-only', () => {
    /* No tab stop on a readout, so the tooltip is unreachable without a
       mouse. The label is what makes that acceptable rather than a hole. */
    render(<ContextRing usage={usage()} />);
    expect(ring()?.hasAttribute('tabindex')).toBe(false);
    expect(ring()?.getAttribute('aria-label')).toContain('24.1k of 258k');
  });

  it('shows a count that overshot its window as its own state, not as a full ring', async () => {
    render(<ContextRing usage={usage({ used_tokens: 2_361_529, context_window: 258_400, percent: null })} />);
    expect(ring()?.getAttribute('data-nc-context-ring')).toBe('over');
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
    /* No decimal above 100k: `258.4k` spends a character on precision that
       changes no decision. */
    expect(formatTokens(258_400)).toBe('258k');
  });
});
