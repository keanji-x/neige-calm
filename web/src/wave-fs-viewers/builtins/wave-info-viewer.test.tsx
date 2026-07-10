import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  __resetWaveFsViewerRegistryForTest,
  registerWaveFsViewer,
} from '../registry';
import { useWaveFsViewer } from '../useWaveFsViewer';
import { WaveInfoViewer } from './wave-info-viewer';

const Component = WaveInfoViewer.Component;

afterEach(() => {
  vi.restoreAllMocks();
  __resetWaveFsViewerRegistryForTest();
});

describe('WaveInfoViewer', () => {
  it('renders wave title, ids, lifecycle, cwd, sort, and timestamps', () => {
    vi.spyOn(Date, 'now').mockReturnValue(
      new Date('2026-06-10T12:00:00Z').getTime(),
    );

    render(
      <Component
        path="wave.json"
        raw="{}"
        data={{
          title: 'Spec wave',
          id: 'wave_1',
          cove_id: 'cove_1',
          lifecycle: 'working',
          cwd: '/repo/neige-calm',
          workflow_id: null,
          workflow_input: null,
          sort: 7,
          archived_at: new Date('2026-06-10T10:00:00Z').getTime(),
          pinned_at: new Date('2026-06-10T11:55:00Z').getTime(),
          terminal_at: null,
          created_at: 0,
          updated_at: 0,
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Spec wave' })).toHaveClass(
      'wave-fs-viewer-primary',
    );
    expect(screen.getByText('wave_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('cove_1')).toHaveClass('wave-fs-viewer-mono');
    expect(screen.getByText('working')).toHaveAttribute('data-tone', 'accent');
    expect(screen.getByText('/repo/neige-calm')).toHaveClass(
      'wave-fs-viewer-break',
    );
    expect(screen.getByText('sort 7')).toBeInTheDocument();
    expect(screen.getByText('Archived 2h ago')).toBeInTheDocument();
    expect(screen.getByText('Pinned 5m ago')).toBeInTheDocument();
  });

  it('hides null timestamp fields and renders empty cwd fallback', () => {
    render(
      <Component
        path="wave.json"
        raw="{}"
        data={{
          title: 'Bare wave',
          id: 'wave_min',
          cove_id: 'cove_min',
          lifecycle: 'draft',
          cwd: '',
          workflow_id: null,
          workflow_input: null,
          sort: 0,
          archived_at: null,
          pinned_at: null,
          terminal_at: null,
          created_at: 0,
          updated_at: 0,
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Bare wave' })).toBeTruthy();
    expect(screen.getByText('sort 0')).toBeInTheDocument();
    expect(screen.getByText('-')).toHaveClass('wave-fs-viewer-break');
    expect(screen.queryByText(/Archived/)).toBeNull();
    expect(screen.queryByText(/Pinned/)).toBeNull();
  });

  it('renders an untitled wave.json through the rich viewer', () => {
    const raw = JSON.stringify({
      title: '',
      id: 'wave_untitled',
      cove_id: 'cove_untitled',
      lifecycle: 'working',
      cwd: '/repo/neige-calm',
      workflow_id: null,
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerWaveFsViewer(WaveInfoViewer);

    render(<ResolvedWaveFsViewer path="wave.json" raw={raw} />);

    expect(
      screen.getByRole('heading', { name: 'Untitled wave' }),
    ).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('wave_untitled')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('defaults missing workflow_id on legacy wave.json snapshots', () => {
    const raw = JSON.stringify({
      title: 'Legacy wave',
      id: 'wave_legacy',
      cove_id: 'cove_legacy',
      lifecycle: 'working',
      cwd: '/repo/neige-calm',
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerWaveFsViewer(WaveInfoViewer);

    render(<ResolvedWaveFsViewer path="wave.json" raw={raw} />);

    expect(screen.getByRole('heading', { name: 'Legacy wave' })).toBeTruthy();
    expect(screen.getByText('wave_legacy')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('falls back to raw when wave.json is missing lifecycle', () => {
    const raw = JSON.stringify({
      title: 'Legacy wave',
      id: 'wave_legacy',
      cove_id: 'cove_legacy',
      cwd: '/repo/neige-calm',
      sort: 0,
      archived_at: null,
      pinned_at: null,
      terminal_at: null,
      created_at: 0,
      updated_at: 0,
    });
    registerWaveFsViewer(WaveInfoViewer);

    render(<ResolvedWaveFsViewer path="wave.json" raw={raw} />);

    expect(screen.getByTestId('code-pane')).toHaveTextContent(raw);
    expect(screen.queryByRole('heading', { name: 'Legacy wave' })).toBeNull();
  });

  it('throws when required fields are missing', () => {
    expect(() => WaveInfoViewer.parse('{"id":"wave_1"}')).toThrow();
    expect(() => WaveInfoViewer.parse('[]')).toThrow();
  });
});

function ResolvedWaveFsViewer({
  path,
  raw,
}: {
  path: string;
  raw: string;
}) {
  const resolved = useWaveFsViewer(path, raw);
  if (!resolved) {
    return <pre data-testid="code-pane">{raw}</pre>;
  }

  const { Viewer, data } = resolved;
  return <Viewer path={path} raw={raw} data={data} />;
}
