import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { TrackFileTree } from './track-file-tree';
import { useState } from '../shared/state';

type MockRoute = {
  status?: number;
  body: unknown;
};

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { retry: false },
    },
  });
}

function renderWithClient(ui: ReactNode) {
  const client = makeClient();
  return render(
    <QueryClientProvider client={client}>{ui}</QueryClientProvider>,
  );
}

function ControlledTrackFileTree({
  trackId,
  ariaLabel,
  fallback,
  showHidden,
  onChange,
}: {
  trackId: string;
  ariaLabel?: string;
  fallback?: ReactNode;
  showHidden?: boolean;
  onChange?: (path: string | null) => void;
}) {
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const handleSelectedPathChange = (path: string | null) => {
    setSelectedPath(path);
    onChange?.(path);
  };

  return (
    <TrackFileTree
      trackId={trackId}
      selectedPath={selectedPath}
      onSelectedPathChange={handleSelectedPathChange}
      ariaLabel={ariaLabel}
      showHidden={showHidden}
      fallback={fallback}
    />
  );
}

function ToggleableHiddenTrackFileTree({ trackId }: { trackId: string }) {
  const [showHidden, setShowHidden] = useState(true);
  return (
    <>
      <button type="button" onClick={() => setShowHidden(false)}>
        Hide dotfiles
      </button>
      <ControlledTrackFileTree trackId={trackId} showHidden={showHidden} />
    </>
  );
}

function installFetch(routes: Record<string, MockRoute>) {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = new URL(String(input), 'http://localhost');
    const logicalPath = url.searchParams.get('path');
    const key =
      logicalPath === null ? url.pathname : `${url.pathname}?path=${logicalPath}`;
    const route = routes[key];
    if (!route) {
      throw new Error(`unmocked fetch: ${key}`);
    }
    return new Response(JSON.stringify(route.body), {
      status: route.status ?? 200,
      headers: { 'content-type': 'application/json' },
    });
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

describe('TrackFileTree', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders root entries in a labelled tree', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: 'report.md', kind: 'file' },
          { name: 'track.json', kind: 'file' },
        ],
      },
    });

    renderWithClient(
      <ControlledTrackFileTree trackId="track_1" ariaLabel="Files" />,
    );

    expect(screen.getByRole('tree', { name: 'Files' })).toBeInTheDocument();
    expect(await screen.findByRole('treeitem', { name: /report\.md/ })).toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /track\.json/ })).toBeTruthy();
  });

  it('hides dot-prefixed entries by default at each directory level', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: '.internal/', kind: 'dir' },
          { name: 'cards/', kind: 'dir', size: 2 },
          { name: 'report.md', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/ls?path=cards': {
        body: [
          { name: '.meta.json', kind: 'file' },
          { name: 'events.json', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/cat?path=cards/index.json': {
        body: {
          content: '[]',
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<ControlledTrackFileTree trackId="track_1" />);

    const cards = await screen.findByRole('treeitem', { name: /cards\// });
    expect(screen.queryByRole('treeitem', { name: /\.internal\// })).toBeNull();
    expect(screen.getByRole('treeitem', { name: /report\.md/ })).toBeTruthy();

    fireEvent.click(cards);

    expect(
      await screen.findByRole('treeitem', { name: /events\.json/ }),
    ).toBeTruthy();
    expect(screen.queryByRole('treeitem', { name: /\.meta\.json/ })).toBeNull();
  });

  it('shows dot-prefixed entries when showHidden is true', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: '.internal/', kind: 'dir' },
          { name: 'cards/', kind: 'dir', size: 2 },
          { name: 'report.md', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/ls?path=cards': {
        body: [
          { name: '.meta.json', kind: 'file' },
          { name: 'events.json', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/cat?path=cards/index.json': {
        body: {
          content: '[]',
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(
      <ControlledTrackFileTree trackId="track_1" showHidden={true} />,
    );

    expect(await screen.findByRole('treeitem', { name: /\.internal\// }))
      .toBeTruthy();
    const cards = screen.getByRole('treeitem', { name: /cards\// });
    expect(screen.getByRole('treeitem', { name: /report\.md/ })).toBeTruthy();

    fireEvent.click(cards);

    expect(await screen.findByRole('treeitem', { name: /\.meta\.json/ }))
      .toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /events\.json/ })).toBeTruthy();
  });

  it('restores the root tab stop when the focused row becomes hidden', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: '.internal', kind: 'file' },
          { name: 'report.md', kind: 'file' },
          { name: 'track.json', kind: 'file' },
        ],
      },
    });

    renderWithClient(<ToggleableHiddenTrackFileTree trackId="track_1" />);

    const dotfile = await screen.findByRole('treeitem', { name: /\.internal/ });
    dotfile.focus();
    expect(dotfile).toHaveFocus();

    fireEvent.click(screen.getByRole('button', { name: 'Hide dotfiles' }));

    expect(screen.queryByRole('treeitem', { name: /\.internal/ })).toBeNull();
    expect(screen.getByRole('treeitem', { name: /report\.md/ }))
      .toHaveAttribute('tabindex', '0');
  });

  it('expands and collapses directories while resolving card kind labels', async () => {
    const cardId = 'card_abc123456789';
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: 'cards/', kind: 'dir', size: 1 },
          { name: 'report.md', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/ls?path=cards': {
        body: [
          { name: 'index.json', kind: 'file' },
          { name: `${cardId}/`, kind: 'dir' },
        ],
      },
      '/api/tracks/track_1/files/cat?path=cards/index.json': {
        body: {
          content: JSON.stringify([{ id: cardId, kind: 'codex' }]),
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<ControlledTrackFileTree trackId="track_1" />);

    const cards = await screen.findByRole('treeitem', { name: /cards\// });
    fireEvent.click(cards);

    expect(cards).toHaveAttribute('aria-expanded', 'true');
    expect(await screen.findByRole('treeitem', { name: /index\.json/ })).toBeTruthy();
    expect(
      await screen.findByRole('treeitem', { name: /codex card_abc/ }),
    ).toBeTruthy();

    fireEvent.click(cards);
    expect(cards).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('treeitem', { name: /index\.json/ })).toBeNull();
  });

  it('supports Arrow, Home, and End keyboard navigation', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: 'cards/', kind: 'dir', size: 1 },
          { name: 'report.md', kind: 'file' },
          { name: 'track.json', kind: 'file' },
        ],
      },
      '/api/tracks/track_1/files/ls?path=cards': {
        body: [{ name: 'index.json', kind: 'file' }],
      },
      '/api/tracks/track_1/files/cat?path=cards/index.json': {
        body: {
          content: '[{"id":"card_one","kind":"codex"}]',
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<ControlledTrackFileTree trackId="track_1" />);

    const cards = await screen.findByRole('treeitem', { name: /cards\// });
    const report = screen.getByRole('treeitem', { name: /report\.md/ });
    const track = screen.getByRole('treeitem', { name: /track\.json/ });

    cards.focus();
    fireEvent.keyDown(cards, { key: 'ArrowDown' });
    expect(report).toHaveFocus();

    fireEvent.keyDown(report, { key: 'End' });
    expect(track).toHaveFocus();

    fireEvent.keyDown(track, { key: 'Home' });
    expect(cards).toHaveFocus();

    fireEvent.keyDown(cards, { key: 'ArrowRight' });
    expect(cards).toHaveAttribute('aria-expanded', 'true');
    const index = await screen.findByRole('treeitem', { name: /index\.json/ });

    fireEvent.keyDown(cards, { key: 'ArrowRight' });
    expect(index).toHaveFocus();

    fireEvent.keyDown(index, { key: 'ArrowLeft' });
    expect(cards).toHaveFocus();

    fireEvent.keyDown(cards, { key: 'ArrowLeft' });
    expect(cards).toHaveAttribute('aria-expanded', 'false');
  });

  it('calls selectedPath changes and marks the active file', async () => {
    const onChange = vi.fn();
    installFetch({
      '/api/tracks/track_1/files/ls': {
        body: [
          { name: 'report.md', kind: 'file' },
          { name: 'track.json', kind: 'file' },
        ],
      },
    });

    renderWithClient(
      <ControlledTrackFileTree trackId="track_1" onChange={onChange} />,
    );

    const report = await screen.findByRole('treeitem', { name: /report\.md/ });
    const track = screen.getByRole('treeitem', { name: /track\.json/ });

    fireEvent.click(report);
    expect(onChange).toHaveBeenCalledWith('report.md');
    expect(report).toHaveAttribute('aria-selected', 'true');
    expect(track).toHaveAttribute('aria-selected', 'false');

    fireEvent.click(track);
    expect(onChange).toHaveBeenCalledWith('track.json');
    expect(report).toHaveAttribute('aria-selected', 'false');
    expect(track).toHaveAttribute('aria-selected', 'true');
  });

  it('renders the root fallback when the file list is empty', async () => {
    installFetch({
      '/api/tracks/track_1/files/ls': { body: [] },
    });

    renderWithClient(
      <ControlledTrackFileTree
        trackId="track_1"
        fallback={<div>No files yet.</div>}
      />,
    );

    expect(screen.getByRole('tree', { name: 'Track files' })).toBeInTheDocument();
    expect(await screen.findByText('No files yet.')).toBeInTheDocument();
  });
});
