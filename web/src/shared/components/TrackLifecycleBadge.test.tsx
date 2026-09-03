// Unit tests for <TrackLifecycleBadge> (issue #145).
//
// The badge is the user-visible projection of the kernel's track
// lifecycle contract. These tests pin the label vocabulary, the
// status-pill color mapping (running / waiting / neutral), and the
// `compact` variant's no-dot rendering.

import { render, screen } from '@testing-library/react';
import { describe, it, expect } from 'vitest';
import { TrackLifecycleBadge, lifecycleLabel } from './TrackLifecycleBadge';
import type { TrackLifecycle } from '../../types';

describe('TrackLifecycleBadge', () => {
  it('renders the label for each lifecycle state', () => {
    const cases: Array<[TrackLifecycle, string]> = [
      ['draft', 'Draft'],
      ['planning', 'Planning'],
      ['dispatching', 'Dispatching'],
      ['working', 'Working'],
      ['blocked', 'Blocked'],
      ['reviewing', 'In review'],
      ['done', 'Done'],
      ['canceled', 'Canceled'],
      ['failed', 'Failed'],
    ];
    for (const [state, label] of cases) {
      const { unmount } = render(<TrackLifecycleBadge lifecycle={state} />);
      expect(
        screen.getByLabelText(`Track lifecycle: ${label}`),
      ).toBeInTheDocument();
      unmount();
    }
  });

  it('exposes `lifecycleLabel` as a pure helper for grouping/sorting sites', () => {
    expect(lifecycleLabel('reviewing')).toBe('In review');
    expect(lifecycleLabel('done')).toBe('Done');
  });

  it('marks active states with the `running` pill modifier', () => {
    for (const state of ['planning', 'dispatching', 'working'] as const) {
      const { container, unmount } = render(
        <TrackLifecycleBadge lifecycle={state} />,
      );
      const pill = container.firstChild as HTMLElement;
      expect(pill.className).toContain('status-pill');
      expect(pill.className).toContain('running');
      unmount();
    }
  });

  it('marks attention-needed states with the `waiting` pill modifier', () => {
    for (const state of ['blocked', 'reviewing', 'failed'] as const) {
      const { container, unmount } = render(
        <TrackLifecycleBadge lifecycle={state} />,
      );
      const pill = container.firstChild as HTMLElement;
      expect(pill.className).toContain('status-pill');
      expect(pill.className).toContain('waiting');
      unmount();
    }
  });

  it('renders neutral (no color modifier) for calm states', () => {
    for (const state of ['draft', 'done', 'canceled'] as const) {
      const { container, unmount } = render(
        <TrackLifecycleBadge lifecycle={state} />,
      );
      const pill = container.firstChild as HTMLElement;
      expect(pill.className).toContain('status-pill');
      // The bare `status-pill` class with no `running` / `waiting`
      // modifier — leading-edge calm dim style.
      expect(pill.className).not.toContain('running');
      expect(pill.className).not.toContain('waiting');
      unmount();
    }
  });

  it('compact variant suppresses the leading status dot', () => {
    const { container } = render(
      <TrackLifecycleBadge lifecycle="working" compact />,
    );
    const pill = container.firstChild as HTMLElement;
    // No `.status-pill-dot` child rendered in compact mode.
    expect(pill.querySelector('.status-pill-dot')).toBeNull();
  });

  it('exposes the raw lifecycle name on `data-lifecycle` for tests / CSS', () => {
    const { container } = render(
      <TrackLifecycleBadge lifecycle="reviewing" />,
    );
    const pill = container.firstChild as HTMLElement;
    expect(pill.getAttribute('data-lifecycle')).toBe('reviewing');
  });
});
