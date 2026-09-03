// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { TrackLifecycleBadge } from './public.tsx';

afterEach(cleanup);

describe('TrackLifecycleBadge', () => {
  it('reads the phrase from core rather than a local table', () => {
    render(<TrackLifecycleBadge lifecycle="reviewing" />);
    expect(screen.getByRole('img', { name: 'Track lifecycle: In review' })).toBeTruthy();
  });

  it('marks blocked, reviewing and failed as the attention treatment', () => {
    for (const lifecycle of ['blocked', 'reviewing', 'failed'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} />);
      expect(screen.getByRole('img').getAttribute('data-nc-lifecycle-tone')).toBe('attention');
    }
  });

  it('marks planning, dispatching and working as the running treatment', () => {
    for (const lifecycle of ['planning', 'dispatching', 'working'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} />);
      expect(screen.getByRole('img').getAttribute('data-nc-lifecycle-tone')).toBe('running');
    }
  });

  it('leaves draft, done and canceled neutral', () => {
    for (const lifecycle of ['draft', 'done', 'canceled'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} />);
      expect(screen.getByRole('img').getAttribute('data-nc-lifecycle-tone')).toBe('neutral');
    }
  });

  it('keeps the label but drops the dot when compact', () => {
    const { container } = render(<TrackLifecycleBadge lifecycle="working" compact />);
    expect(screen.getByRole('img', { name: 'Track lifecycle: Working' })).toBeTruthy();
    expect(screen.getByText('Working')).toBeTruthy();
    expect(container.querySelectorAll('span[aria-hidden="true"]').length).toBe(0);
  });

  it('renders the dot when not compact', () => {
    const { container } = render(<TrackLifecycleBadge lifecycle="working" />);
    expect(container.querySelectorAll('span[aria-hidden="true"]').length).toBe(1);
  });
});
