// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { lifecycleLabel } from '../../../../../core/domain/track.ts';
import { TrackLifecycleBadge } from './public.tsx';

afterEach(cleanup);

describe('TrackLifecycleBadge', () => {
  it('reads the phrase from core rather than a local table', () => {
    render(<TrackLifecycleBadge lifecycle="reviewing" canResume onResume={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Track lifecycle: In review' })).toBeTruthy();
  });

  it('marks blocked, reviewing and failed as the attention treatment', () => {
    for (const lifecycle of ['blocked', 'reviewing', 'failed'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} canResume={false} onResume={vi.fn()} />);
      expect(screen.getByTestId('track-lifecycle').getAttribute('data-nc-lifecycle-tone')).toBe('attention');
    }
  });

  it('marks planning, dispatching and working as the running treatment', () => {
    for (const lifecycle of ['planning', 'dispatching', 'working'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} canResume={false} onResume={vi.fn()} />);
      expect(screen.getByTestId('track-lifecycle').getAttribute('data-nc-lifecycle-tone')).toBe('running');
    }
  });

  it('leaves draft, done and canceled neutral', () => {
    for (const lifecycle of ['draft', 'done', 'canceled'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} canResume={false} onResume={vi.fn()} />);
      expect(screen.getByTestId('track-lifecycle').getAttribute('data-nc-lifecycle-tone')).toBe('neutral');
    }
  });

  it('renders every lifecycle as a quiet button without a decorative status dot', () => {
    const { container } = render(
      <TrackLifecycleBadge lifecycle="working" canResume={false} onResume={vi.fn()} />,
    );
    expect(screen.getByRole('button', { name: 'Track lifecycle: Working' })).toBeTruthy();
    expect(screen.getByText('Working')).toBeTruthy();
    expect(container.querySelectorAll('span[aria-hidden="true"]').length).toBe(0);
  });

  it('offers exactly Resume work for a recoverable lifecycle', async () => {
    const onResume = vi.fn();
    render(<TrackLifecycleBadge lifecycle="done" canResume onResume={onResume} />);
    await userEvent.click(screen.getByRole('button', { name: 'Track lifecycle: Done' }));
    const action = screen.getByRole('menuitem', { name: /Resume work/ });
    expect(screen.getAllByRole('menuitem')).toHaveLength(1);
    await userEvent.click(action);
    expect(onResume).toHaveBeenCalledOnce();
  });

  it('light-dismisses on Escape', async () => {
    render(<TrackLifecycleBadge lifecycle="done" canResume onResume={vi.fn()} />);
    const trigger = screen.getByRole('button', { name: 'Track lifecycle: Done' });

    await userEvent.click(trigger);
    expect(screen.getByRole('menu')).toBeTruthy();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('keeps non-recoverable lifecycle buttons inert', () => {
    for (const lifecycle of ['draft', 'planning', 'dispatching', 'working'] as const) {
      cleanup();
      render(<TrackLifecycleBadge lifecycle={lifecycle} canResume={false} onResume={vi.fn()} />);
      const button = screen.getByRole('button', {
        name: `Track lifecycle: ${lifecycleLabel(lifecycle)}`,
      });
      expect(button.hasAttribute('disabled')).toBe(true);
      expect(button.getAttribute('aria-haspopup')).toBeNull();
    }
  });

  it('keeps a server-denied terminal track inert instead of guessing from Done', () => {
    render(<TrackLifecycleBadge lifecycle="done" canResume={false} onResume={vi.fn()} />);
    const button = screen.getByRole('button', { name: 'Track lifecycle: Done' });
    expect(button.hasAttribute('disabled')).toBe(true);
    expect(button.getAttribute('aria-haspopup')).toBeNull();
  });
});
