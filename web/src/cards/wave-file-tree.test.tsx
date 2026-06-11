import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { WaveFileTree } from './wave-file-tree';
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

function ControlledWaveFileTree({
  waveId,
  ariaLabel,
  fallback,
  onChange,
}: {
  waveId: string;
  ariaLabel?: string;
  fallback?: ReactNode;
  onChange?: (path: string | null) => void;
}) {
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const handleSelectedPathChange = (path: string | null) => {
    setSelectedPath(path);
    onChange?.(path);
  };

  return (
    <WaveFileTree
      waveId={waveId}
      selectedPath={selectedPath}
      onSelectedPathChange={handleSelectedPathChange}
      ariaLabel={ariaLabel}
      fallback={fallback}
    />
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

describe('WaveFileTree', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders root entries in a labelled tree', async () => {
    installFetch({
      '/api/waves/wave_1/files/ls': {
        body: [
          { name: 'report.md', kind: 'file' },
          { name: 'wave.json', kind: 'file' },
        ],
      },
    });

    renderWithClient(
      <ControlledWaveFileTree waveId="wave_1" ariaLabel="Files" />,
    );

    expect(screen.getByRole('tree', { name: 'Files' })).toBeInTheDocument();
    expect(await screen.findByRole('treeitem', { name: /report\.md/ })).toBeTruthy();
    expect(screen.getByRole('treeitem', { name: /wave\.json/ })).toBeTruthy();
  });

  it('expands and collapses directories while resolving card kind labels', async () => {
    const cardId = 'card_abc123456789';
    installFetch({
      '/api/waves/wave_1/files/ls': {
        body: [
          { name: 'cards/', kind: 'dir', size: 1 },
          { name: 'report.md', kind: 'file' },
        ],
      },
      '/api/waves/wave_1/files/ls?path=cards': {
        body: [
          { name: 'index.json', kind: 'file' },
          { name: `${cardId}/`, kind: 'dir' },
        ],
      },
      '/api/waves/wave_1/files/cat?path=cards/index.json': {
        body: {
          content: JSON.stringify([{ id: cardId, kind: 'codex' }]),
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<ControlledWaveFileTree waveId="wave_1" />);

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
      '/api/waves/wave_1/files/ls': {
        body: [
          { name: 'cards/', kind: 'dir', size: 1 },
          { name: 'report.md', kind: 'file' },
          { name: 'wave.json', kind: 'file' },
        ],
      },
      '/api/waves/wave_1/files/ls?path=cards': {
        body: [{ name: 'index.json', kind: 'file' }],
      },
      '/api/waves/wave_1/files/cat?path=cards/index.json': {
        body: {
          content: '[{"id":"card_one","kind":"codex"}]',
          content_type: 'application/json',
        },
      },
    });

    renderWithClient(<ControlledWaveFileTree waveId="wave_1" />);

    const cards = await screen.findByRole('treeitem', { name: /cards\// });
    const report = screen.getByRole('treeitem', { name: /report\.md/ });
    const wave = screen.getByRole('treeitem', { name: /wave\.json/ });

    cards.focus();
    fireEvent.keyDown(cards, { key: 'ArrowDown' });
    expect(report).toHaveFocus();

    fireEvent.keyDown(report, { key: 'End' });
    expect(wave).toHaveFocus();

    fireEvent.keyDown(wave, { key: 'Home' });
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
      '/api/waves/wave_1/files/ls': {
        body: [
          { name: 'report.md', kind: 'file' },
          { name: 'wave.json', kind: 'file' },
        ],
      },
    });

    renderWithClient(
      <ControlledWaveFileTree waveId="wave_1" onChange={onChange} />,
    );

    const report = await screen.findByRole('treeitem', { name: /report\.md/ });
    const wave = screen.getByRole('treeitem', { name: /wave\.json/ });

    fireEvent.click(report);
    expect(onChange).toHaveBeenCalledWith('report.md');
    expect(report).toHaveAttribute('aria-selected', 'true');
    expect(wave).toHaveAttribute('aria-selected', 'false');

    fireEvent.click(wave);
    expect(onChange).toHaveBeenCalledWith('wave.json');
    expect(report).toHaveAttribute('aria-selected', 'false');
    expect(wave).toHaveAttribute('aria-selected', 'true');
  });

  it('renders the root fallback when the file list is empty', async () => {
    installFetch({
      '/api/waves/wave_1/files/ls': { body: [] },
    });

    renderWithClient(
      <ControlledWaveFileTree
        waveId="wave_1"
        fallback={<div>No files yet.</div>}
      />,
    );

    expect(screen.getByRole('tree', { name: 'Wave files' })).toBeInTheDocument();
    expect(await screen.findByText('No files yet.')).toBeInTheDocument();
  });
});
