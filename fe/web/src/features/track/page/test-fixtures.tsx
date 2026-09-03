// Shared fixtures for the TrackPage behaviour and contract suites.
//
// `Track` carries the plugin activity fields, so the factory spreads
// `NEUTRAL_ACTIVITY` — "no plugin has posted anything" is a value, not a hole.

import { render, type RenderResult } from '@testing-library/react';
import { vi } from 'vitest';

import { NEUTRAL_ACTIVITY, type CardWire, type Track } from '../../../../../core/domain/track.ts';
import { useState } from '../../../ui/state/public.ts';
import { TrackPage, type TrackPageProps } from './public.tsx';

type Panel = NonNullable<TrackPageProps['panel']>;

export function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'working', cwd: '/tmp/alpha',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

export function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'card-1', track_id: 'w1', kind: 'terminal', title: 'Main terminal', sort: 1,
    payload: null, deletable: true, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

/*
 * `TrackPage` is a pure renderer: since #1191 §2.4 the secondary panel is a prop
 * fed from `?panel=`, so these suites need something to hold it. This holder is
 * *only* a holder — the production owner is `app/router`'s
 * `useTrackPanelNavigation`, and `app/router/mobile-report-navigation.test.tsx`
 * drives that one through a real router. A test that passes `panel` explicitly
 * opts out and gets the fixed value it asked for.
 */
function PanelHost({ props }: { props: TrackPageProps }) {
  const [panel, setPanel] = useState<Panel | null>(props.panel ?? null);
  return (
    <TrackPage
      {...props}
      panel={panel}
      onOpenPanel={(next) => { setPanel(next); props.onOpenPanel?.(next); }}
      onClosePanel={() => { setPanel(null); props.onClosePanel?.(); }}
    />
  );
}

export function renderPage(overrides: Partial<TrackPageProps> = {}): RenderResult {
  const props: TrackPageProps = {
    track: track(),
    cards: [],
    /* A track with no report has no tasks, which is the honest default — the
       TASKS cases below pass their own. */
    tasks: [],
    onRenameTrack: vi.fn(),
    onDeleteTrack: vi.fn(),
    ...overrides,
  };
  return render(<PanelHost props={props} />);
}
