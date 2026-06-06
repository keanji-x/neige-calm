// File-viewer card tests. CodeMirror itself is lazy-loaded and mocked here;
// these tests pin the kernel adapter and the card's data-fetching/render
// contract without pulling editor packages into jsdom.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import type { KernelCard, KernelOverlay, NewOverlayBody } from '../../api/wire';
import {
  CardInstanceProvider,
  useCardInstanceCtx,
} from '../registry';

vi.mock('../../app/theme', () => ({
  useTheme: () => ({ resolved: 'light' }),
}));

vi.mock('./file-viewer-codemirror', () => ({
  CodePane: ({ path, text }: { path: string; text: string }) => (
    <pre className="cm-scroller" data-testid="code-pane" data-path={path}>
      {text}
    </pre>
  ),
  DiffPane: ({
    path,
    headText,
    workingText,
  }: {
    path: string;
    headText: string | null;
    workingText: string | null;
  }) => (
    <pre
      className="file-viewer-merge"
      data-testid="diff-pane"
      data-path={path}
    >
      {headText ?? ''}
      {' -> '}
      {workingText ?? ''}
    </pre>
  ),
}));

vi.mock('../../api/calm', async () => {
  const actual =
    await vi.importActual<typeof import('../../api/calm')>('../../api/calm');
  return {
    ...actual,
    listDir: vi.fn(),
    readFile: vi.fn(),
    gitStatus: vi.fn(),
    gitDiff: vi.fn(),
    listOverlays: vi.fn(),
    upsertOverlay: vi.fn(),
  };
});

import { FileViewerEntry } from './file-viewer';
import * as api from '../../api/calm';
import { overlayStateQueryKey } from '../../hooks/useOverlayState';

function makeKernelCard(over: Partial<KernelCard> = {}): KernelCard {
  return {
    id: 'file_1',
    wave_id: 'wave_1',
    kind: 'file-viewer',
    sort: 0,
    payload: { path: '/repo/src/main.ts' },
    deletable: true,
    created_at: 1000,
    updated_at: 2000,
    ...over,
  };
}

function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });
}

function renderWithClient(ui: ReactNode, client = makeClient()) {
  const providerCard = {
    type: 'file-viewer' as const,
    id: 'file_1',
    path: '/repo/src/main.ts',
  };
  return render(
    <QueryClientProvider client={client}>
      <CardInstanceProvider
        cardId={providerCard.id}
        deletable
        card={providerCard}
      >
        {ui}
      </CardInstanceProvider>
    </QueryClientProvider>,
  );
}

function PaneSlotProbe({
  onSlot,
}: {
  onSlot: (slot: { current: HTMLElement | null }) => void;
}) {
  const [slot] = useCardInstanceCtx().useInstance<{
    current: HTMLElement | null;
  }>('fvPaneRef', { current: null });
  useEffect(() => {
    onSlot(slot);
  }, [onSlot, slot]);
  return null;
}

describe('FileViewerEntry.fromKernel', () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it('claims kind=file-viewer with a path payload', () => {
    const out = FileViewerEntry.fromKernel!(makeKernelCard());
    expect(out).toEqual({
      type: 'file-viewer',
      id: 'file_1',
      path: '/repo/src/main.ts',
    });
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('returns null for other kinds', () => {
    const out = FileViewerEntry.fromKernel!(makeKernelCard({ kind: 'terminal' }));
    expect(out).toBeNull();
    expect(warnSpy).not.toHaveBeenCalled();
  });

  it('returns null and warns for an invalid payload', () => {
    const out = FileViewerEntry.fromKernel!(makeKernelCard({ payload: {} }));
    expect(out).toBeNull();
    expect(warnSpy).toHaveBeenCalled();
  });
});

describe('FileViewerCard rendering', () => {
  const overlayStore = new Map<string, KernelOverlay>();

  beforeEach(() => {
    overlayStore.clear();
    try {
      window.localStorage.clear();
    } catch {
      // ignore
    }
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/src/main.ts') {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo/src',
        parent: '/repo',
        entries: [{ name: 'main.ts', is_dir: false }],
      };
    });
    vi.mocked(api.readFile).mockResolvedValue({
      path: '/repo/src/main.ts',
      size: 20,
      text: 'console.log("hi");\n',
      truncated: false,
    });
    vi.mocked(api.gitStatus).mockResolvedValue({
      repo_root: '/repo',
      files: [{ path: 'src/main.ts', status: 'modified' }],
    });
    vi.mocked(api.gitDiff).mockResolvedValue({
      path: 'src/main.ts',
      status: 'modified',
      head_text: 'old\n',
      working_text: 'new\n',
      truncated: false,
    });
    vi.mocked(api.listOverlays).mockImplementation(async (entityKind, entityId) =>
      [...overlayStore.values()].filter(
        (overlay) =>
          overlay.entity_kind === entityKind && overlay.entity_id === entityId,
      ),
    );
    vi.mocked(api.upsertOverlay).mockImplementation(
      async (body: NewOverlayBody) => {
        const overlay: KernelOverlay = {
          id: 'ov-file-nav',
          plugin_id: body.plugin_id,
          entity_kind: body.entity_kind,
          entity_id: body.entity_id,
          kind: body.kind,
          payload: body.payload,
          updated_at: 1,
        };
        overlayStore.set(
          `${body.plugin_id}:${body.entity_kind}:${body.entity_id}:${body.kind}`,
          overlay,
        );
        return overlay;
      },
    );
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('loads the parent directory for an initial file path and renders read-only code', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    const code = await screen.findByTestId('code-pane');
    expect(code).toHaveTextContent('console.log("hi");');
    expect(api.listDir).toHaveBeenCalledWith('/repo/src/main.ts');
    expect(api.listDir).toHaveBeenCalledWith('/repo/src');
    expect(api.readFile).toHaveBeenCalledWith('/repo/src/main.ts');

    await act(async () => {
      (await screen.findByRole('tab', { name: 'Diff' })).click();
    });
    expect(await screen.findAllByText('src/main.ts')).not.toHaveLength(0);
    expect(await screen.findByTestId('diff-pane')).toHaveTextContent('old');
  });

  it('renders image files without reading them as UTF-8 text', async () => {
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/assets/pixel.png') {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo/assets',
        parent: '/repo',
        entries: [{ name: 'pixel.png', is_dir: false }],
      };
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{
          type: 'file-viewer',
          id: 'file_1',
          path: '/repo/assets/pixel.png',
        }}
      />,
    );

    const img = await screen.findByRole('img', {
      name: '/repo/assets/pixel.png',
    });
    expect(api.readFile).not.toHaveBeenCalled();
    expect(img).toHaveAttribute('src', api.readFileRaw('/repo/assets/pixel.png'));
  });

  it('keeps a canonicalized directory seed unselected', async () => {
    const client = makeClient();
    client.setQueryData(
      overlayStateQueryKey('kernel', 'card', 'file_1', 'file-viewer-nav'),
      {
        schemaVersion: 1,
        tab: 'code',
        folderPath: '/repo/',
        selectedPath: '/repo/',
        diffSelected: null,
      },
    );
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      expect(['/repo/', '/repo']).toContain(path);
      return {
        path: '/repo',
        parent: '/',
        entries: [{ name: 'src', is_dir: true }],
      };
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component card={{ type: 'file-viewer', id: 'file_1', path: '/repo/' }} />,
      client,
    );

    expect(await screen.findByText('/repo')).toBeTruthy();
    expect(await screen.findByText('Select a file to view it.')).toBeTruthy();
    expect(api.readFile).not.toHaveBeenCalled();
  });

  it('walks up from a deleted persisted folder', async () => {
    vi.mocked(api.listOverlays).mockResolvedValue([
      {
        id: 'ov-file-nav',
        plugin_id: 'kernel',
        entity_kind: 'card',
        entity_id: 'file_1',
        kind: 'file-viewer-nav',
        payload: {
          schemaVersion: 1,
          tab: 'code',
          folderPath: '/repo/subdir',
          selectedPath: null,
          diffSelected: null,
        },
        updated_at: 1,
      },
    ]);
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/subdir') {
        throw new api.CalmApiError(404, 'not_found', 'missing directory');
      }
      if (path === '/repo') {
        return {
          path: '/repo',
          parent: '/',
          entries: [{ name: 'main.ts', is_dir: false }],
        };
      }
      throw new api.CalmApiError(404, 'not_found', 'missing directory');
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component card={{ type: 'file-viewer', id: 'file_1', path: '/repo' }} />,
      makeClient(),
    );

    await waitFor(() => expect(api.listDir).toHaveBeenCalledWith('/repo/subdir'));
    await waitFor(() =>
      expect(api.upsertOverlay).toHaveBeenCalledWith({
        plugin_id: 'kernel',
        entity_kind: 'card',
        entity_id: 'file_1',
        kind: 'file-viewer-nav',
        payload: {
          schemaVersion: 1,
          tab: 'code',
          folderPath: '/repo',
          selectedPath: null,
          diffSelected: null,
        },
      }),
    );
    expect(await screen.findByText('/repo')).toBeTruthy();
    expect(screen.queryByText('missing directory')).toBeNull();
  });

  it('collapses and expands the file tree from the toolbar', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    expect(screen.getByLabelText('Files')).toBeTruthy();
    expect(await screen.findByText('main.ts')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: 'Collapse file tree' }));
    expect(screen.queryByLabelText('Files')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Expand file tree' }));
    expect(screen.getByLabelText('Files')).toBeTruthy();
    expect(screen.getByText('main.ts')).toBeTruthy();
  });

  it('updates the declared wheel pane ref when switching active tabs', async () => {
    const Component = FileViewerEntry.Component;
    let paneSlot: { current: HTMLElement | null } | null = null;
    renderWithClient(
      <>
        <Component
          card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
        />
        <PaneSlotProbe
          onSlot={(slot) => {
            paneSlot = slot;
          }}
        />
      </>,
    );

    await waitFor(() =>
      expect(paneSlot?.current).toHaveClass('cm-scroller'),
    );

    fireEvent.click(screen.getByRole('tab', { name: 'Diff' }));
    await waitFor(() =>
      expect(paneSlot?.current).toHaveClass('file-viewer-merge'),
    );

    fireEvent.click(screen.getByRole('tab', { name: 'Code' }));
    await waitFor(() =>
      expect(paneSlot?.current).toHaveClass('cm-scroller'),
    );
  });

  it('restores navigation overlay state after unmounting and remounting the same card', async () => {
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/src') {
        return {
          path: '/repo/src',
          parent: '/repo',
          entries: [
            { name: 'components', is_dir: true },
            { name: 'main.ts', is_dir: false },
          ],
        };
      }
      if (path === '/repo/src/components') {
        return {
          path: '/repo/src/components',
          parent: '/repo/src',
          entries: [{ name: 'Button.tsx', is_dir: false }],
        };
      }
      throw new api.CalmApiError(400, 'bad_request', 'not a directory');
    });
    vi.mocked(api.readFile).mockImplementation(async (path: string) => ({
      path,
      size: 12,
      text: `contents:${path}`,
      truncated: false,
    }));
    vi.mocked(api.gitStatus).mockResolvedValue({
      repo_root: '/repo',
      files: [
        { path: 'src/components/Button.tsx', status: 'modified' },
        { path: 'src/main.ts', status: 'modified' },
      ],
    });
    vi.mocked(api.gitDiff).mockImplementation(async (path: string) => ({
      path: path.replace('/repo/', ''),
      status: 'modified',
      head_text: `old:${path}`,
      working_text: `new:${path}`,
      truncated: false,
    }));

    const Component = FileViewerEntry.Component;
    const card = { type: 'file-viewer' as const, id: 'file_1', path: '/repo/src' };
    const client = makeClient();
    const first = renderWithClient(<Component card={card} />, client);
    await waitFor(() =>
      expect(
        client.getQueryState(
          overlayStateQueryKey('kernel', 'card', 'file_1', 'file-viewer-nav'),
        )?.status,
      ).toBe('success'),
    );

    fireEvent.click(await screen.findByText('components'));
    fireEvent.click(await screen.findByText('Button.tsx'));
    await screen.findByTestId('code-pane');

    fireEvent.click(screen.getByRole('tab', { name: 'Diff' }));
    await screen.findByTestId('diff-pane');
    fireEvent.click(await screen.findByRole('button', { name: /src\/main\.ts/ }));

    await waitFor(() =>
      expect(api.upsertOverlay).toHaveBeenLastCalledWith({
        plugin_id: 'kernel',
        entity_kind: 'card',
        entity_id: 'file_1',
        kind: 'file-viewer-nav',
        payload: {
          schemaVersion: 1,
          tab: 'diff',
          folderPath: '/repo/src/components',
          selectedPath: '/repo/src/components/Button.tsx',
          diffSelected: 'src/main.ts',
        },
      }),
    );

    first.unmount();
    vi.mocked(api.listOverlays).mockClear();
    renderWithClient(<Component card={card} />, makeClient());

    await waitFor(() =>
      expect(api.listOverlays).toHaveBeenCalledWith('card', 'file_1'),
    );
    expect(await screen.findByText('/repo/src/components')).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'Diff' })).toHaveAttribute(
      'aria-selected',
      'true',
    );
    expect(await screen.findByTestId('diff-pane')).toHaveAttribute(
      'data-path',
      'src/main.ts',
    );

    fireEvent.click(screen.getByRole('tab', { name: 'Code' }));
    expect(await screen.findByTestId('code-pane')).toHaveAttribute(
      'data-path',
      '/repo/src/components/Button.tsx',
    );
  });
});
