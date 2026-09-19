// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ActivityIndicator } from './public.tsx';

afterEach(cleanup);

/*
 * #1722 S2b r1 — the spoken counterpart. The marker is decorative by
 * contract; `spoken` is the one way a surface with no owning control gives
 * the verdict an accessible name, and it is opt-in so the S2a contract (the
 * owning control names it) stays the default.
 */
describe('ActivityIndicator spoken', () => {
  it('renders the given text visually hidden after the marker, and only then', () => {
    const { container } = render(<ActivityIndicator state="working" spoken="Working" />);
    const marker = container.querySelector('[data-nc-activity="working"]');
    expect(marker).not.toBeNull();
    const spoken = screen.getByText('Working');
    expect(marker?.nextElementSibling).toBe(spoken);
    expect(spoken.getAttribute('data-nc-activity')).toBeNull();
    expect(spoken.getAttribute('aria-hidden')).toBeNull();
    expect(container.querySelectorAll('[data-nc-activity]')).toHaveLength(1);
  });

  it('says nothing by default', () => {
    const { container } = render(<ActivityIndicator state="working" />);
    expect(container.querySelector('[data-nc-activity="working"]')).not.toBeNull();
    expect(container.textContent).toBe('');
  });

  it('says nothing for quiet even when asked to', () => {
    const { container } = render(<ActivityIndicator state="quiet" spoken="Working" />);
    expect(container.innerHTML).toBe('');
  });
});
