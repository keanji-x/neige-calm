// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { TrackClosedBadge } from './public.tsx';

afterEach(cleanup);

describe('TrackClosedBadge', () => {
  it('renders nothing for an open track', () => {
    const { container } = render(<TrackClosedBadge closedAt={null} />);
    expect(container.childElementCount).toBe(0);
  });

  it('renders an inert Closed status for a closed track', () => {
    const { container } = render(<TrackClosedBadge closedAt={1_700_000_000_000} />);
    expect(screen.getByRole('status', { name: 'Track closed' }).textContent).toBe('Closed');
    expect(container.querySelector('button')).toBeNull();
  });
});
