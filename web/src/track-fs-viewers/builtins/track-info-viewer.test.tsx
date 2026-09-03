import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  __resetTrackFsViewerRegistryForTest,
  registerTrackFsViewer,
} from '../registry';
import { useTrackFsViewer } from '../useTrackFsViewer';
import { TrackInfoViewer } from './track-info-viewer';

const Component = TrackInfoViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
  __resetTrackFsViewerRegistryForTest();
});

describe('TrackInfoViewer', () => {
  it('renders track title, ids, lifecycle, cwd, sort, and timestamps', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="track.json"
        raw="{}"
        data={{
          title: 'Planner track',
          id: 'track_1',
          area_id: 'area_1',
          lifecycle: 'working',
          cwd: '/repo/neige-calm',
          workspace: { kind: 'attached', path: '/repo/neige-calm', frozen_at: 1000 },
          template_id: null,
          plugin_scope: null,
          purpose: null,
          template_input: null,
          sort: 7,
          archived_at: new Date('2026-06-10T10:00:00Z').getTime(),
          pinned_at: new Date('2026-06-10T11:55:00Z').getTime(),
          terminal_at: null,
          created_at: 0,
          updated_at: 0,
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Planner track' })).toHaveClass(
      'track-fs-viewer-primary',
    );
    expect(screen.getByText('track_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('area_1')).toHaveClass('track-fs-viewer-mono');
    expect(screen.getByText('working')).toHaveAttribute('data-tone', 'accent');
    expect(screen.getByText('/repo/neige-calm')).toHaveClass(
      'track-fs-viewer-break',
    );
    expect(screen.getByText('sort 7')).toBeInTheDocument();
    expect(screen.getByText('Archived 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Pinned 5m ago')).toBeInTheDocument();
  });

  it('hides null timestamp fields and renders empty cwd fallback', () => {
    render(
      <Component
        path="track.json"
        raw="{}"
        data={{
          title: 'Bare track',
          id: 'track_min',
          area_id: 'area_min',
          lifecycle: 'draft',
          cwd: '',
          workspace: { kind: 'attached', path: '', frozen_at: null },
          template_id: null,
          plugin_scope: null,
          purpose: null,
          template_input: null,
          sort: 0,
          archived_at: null,
          pinned_at: null,
          terminal_at: null,
          created_at: 0,
          updated_at: 0,
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Bare track' })).toBeTruthy();
    expect(screen.getByText('sort 0')).toBeInTheDocument();
    expect(screen.getByText('-')).toHaveClass('track-fs-viewer-break');
    expect(screen.queryByText(/Archived/)).toBeNull();
    expect(screen.queryByText(/Pinned/)).toBeNull();
  });

  it('renders an untitled track.json through the rich viewer', () => {
    const raw = JSON.stringify({
      title: '',
      id: 'track_untitled',
      area_id: 'area_untitled',
      lifecycle: 'working',
      cwd: '/repo/neige-calm',
      template_id: null,
      plugin_scope: null,
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerTrackFsViewer(TrackInfoViewer);

    render(<ResolvedTrackFsViewer path="track.json" raw={raw} />);

    expect(
      screen.getByRole('heading', { name: 'Untitled track' }),
    ).toHaveClass('track-fs-viewer-primary');
    expect(screen.getByText('track_untitled')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('defaults missing template_id on legacy track.json snapshots', () => {
    const raw = JSON.stringify({
      title: 'Legacy track',
      id: 'track_legacy',
      area_id: 'area_legacy',
      lifecycle: 'working',
      cwd: '/repo/neige-calm',
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerTrackFsViewer(TrackInfoViewer);

    render(<ResolvedTrackFsViewer path="track.json" raw={raw} />);

    expect(screen.getByRole('heading', { name: 'Legacy track' })).toBeTruthy();
    expect(screen.getByText('track_legacy')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('falls back to raw when track.json is missing lifecycle', () => {
    const raw = JSON.stringify({
      title: 'Legacy track',
      id: 'track_legacy',
      area_id: 'area_legacy',
      cwd: '/repo/neige-calm',
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerTrackFsViewer(TrackInfoViewer);

    render(<ResolvedTrackFsViewer path="track.json" raw={raw} />);

    expect(screen.getByTestId('code-pane')).toHaveTextContent(raw);
    expect(screen.queryByRole('heading', { name: 'Legacy track' })).toBeNull();
  });

  it('throws when required fields are missing', () => {
    expect(() => TrackInfoViewer.parse('{"id":"track_1"}')).toThrow();
    expect(() => TrackInfoViewer.parse('[]')).toThrow();
  });
});

function ResolvedTrackFsViewer({
  path,
  raw,
}: {
  path: string;
  raw: string;
}) {
  const resolved = useTrackFsViewer(path, raw);
  if (!resolved) {
    return <pre data-testid="code-pane">{raw}</pre>;
  }

  const { Viewer, data } = resolved;
  return <Viewer path={path} raw={raw} data={data} />;
}
