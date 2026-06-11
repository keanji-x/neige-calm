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
          coveId: 'cove_1',
          lifecycle: 'working',
          cwd: '/repo/neige-calm',
          sort: 7,
          archivedAt: new Date('2026-06-10T10:00:00Z').getTime(),
          pinnedAt: new Date('2026-06-10T11:55:00Z').getTime(),
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

  it('renders placeholders with all optional fields missing', () => {
    render(
      <Component
        path="wave.json"
        raw="{}"
        data={{
          title: 'Bare wave',
          id: 'wave_min',
          coveId: 'cove_min',
          lifecycle: 'draft',
        }}
      />,
    );

    expect(screen.getByRole('heading', { name: 'Bare wave' })).toBeTruthy();
    expect(screen.getByText('sort -')).toBeInTheDocument();
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
    });
    registerWaveFsViewer(WaveInfoViewer);

    render(<ResolvedWaveFsViewer path="wave.json" raw={raw} />);

    expect(
      screen.getByRole('heading', { name: 'Untitled wave' }),
    ).toHaveClass('wave-fs-viewer-primary');
    expect(screen.getByText('wave_untitled')).toBeInTheDocument();
    expect(screen.queryByTestId('code-pane')).not.toBeInTheDocument();
  });

  it('throws when required fields are missing', () => {
    expect(() => WaveInfoViewer.parse('{"id":"wave_1"}')).toThrow(
      /title string/,
    );
    expect(() => WaveInfoViewer.parse('[]')).toThrow(/must be an object/);
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
