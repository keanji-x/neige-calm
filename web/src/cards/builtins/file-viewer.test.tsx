// File-viewer card tests. CodeMirror itself is lazy-loaded and mocked here;
// these tests pin the kernel adapter and the card's data-fetching/render
// contract without pulling editor packages into jsdom.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { useEffect, type ReactNode } from 'react';
import type { KernelCard, KernelOverlay, NewOverlayBody } from '../../api/wire';
import {
  __resetRegistryForTest,
  CardInstanceProvider,
  registerCard,
  useCardInstanceCtx,
} from '../registry';
import {
  __resetCardEntryResolverRegistryForTest,
  resolveCardById,
} from '../resolver';

const mocks = vi.hoisted(() => ({
  dlog: vi.fn(),
}));

vi.mock('../../app/theme', () => ({
  useTheme: () => ({ resolved: 'light' }),
}));

vi.mock('../../util/debug', () => ({
  dlog: mocks.dlog,
}));

import { useEffect as reactUseEffect } from 'react';
import type { PaneSearchAdapter } from './file-viewer-markdown';

// Test-visible captures for the mocked panes. The mocks pretend to be a
// real pane and hand the fake adapter up to LoadedFileContent via
// `onSearchAdapterReady`, so the tests can drive next/prev/setQuery
// without a live CodeMirror or CSS.highlights.
const paneMocks = vi.hoisted(() => {
  const adapters: { code: PaneSearchAdapter | null; markdown: PaneSearchAdapter | null } = {
    code: null,
    markdown: null,
  };
  const slashOpen: {
    code: (() => void) | null;
    markdown: (() => void) | null;
  } = { code: null, markdown: null };
  const setQuerySpies = {
    code: vi.fn(),
    markdown: vi.fn(),
  };
  const nextSpies = {
    code: vi.fn(),
    markdown: vi.fn(),
  };
  const prevSpies = {
    code: vi.fn(),
    markdown: vi.fn(),
  };
  const disposeSpies = {
    code: vi.fn(),
    markdown: vi.fn(),
  };
  return {
    adapters,
    slashOpen,
    setQuerySpies,
    nextSpies,
    prevSpies,
    disposeSpies,
    reset() {
      adapters.code = null;
      adapters.markdown = null;
      slashOpen.code = null;
      slashOpen.markdown = null;
      setQuerySpies.code.mockClear();
      setQuerySpies.markdown.mockClear();
      nextSpies.code.mockClear();
      nextSpies.markdown.mockClear();
      prevSpies.code.mockClear();
      prevSpies.markdown.mockClear();
      disposeSpies.code.mockClear();
      disposeSpies.markdown.mockClear();
    },
  };
});

function makeFakeAdapter(
  kind: 'code' | 'markdown',
  onCount?: (current: number, total: number) => void,
): PaneSearchAdapter {
  return {
    setQuery: (pattern: string) => {
      paneMocks.setQuerySpies[kind](pattern);
      const total = pattern === 'missing' ? 0 : pattern ? 3 : 0;
      onCount?.(total ? 1 : 0, total);
    },
    next: () => {
      paneMocks.nextSpies[kind]();
      onCount?.(2, 3);
    },
    prev: () => {
      paneMocks.prevSpies[kind]();
      onCount?.(1, 3);
    },
    dispose: () => paneMocks.disposeSpies[kind](),
  };
}

vi.mock('./file-viewer-codemirror', () => ({
  CodePane: ({
    path,
    text,
    onSearchAdapterReady,
    onSearchCount,
    onSlashOpen,
  }: {
    path: string;
    text: string;
    onSearchAdapterReady?: (adapter: PaneSearchAdapter | null) => void;
    onSearchCount?: (current: number, total: number) => void;
    onSlashOpen?: () => void;
  }) => {
    reactUseEffect(() => {
      paneMocks.slashOpen.code = onSlashOpen ?? null;
      const adapter = makeFakeAdapter('code', onSearchCount);
      paneMocks.adapters.code = adapter;
      onSearchAdapterReady?.(adapter);
      return () => {
        onSearchAdapterReady?.(null);
        paneMocks.adapters.code = null;
        paneMocks.slashOpen.code = null;
      };
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [path, text]);
    return (
      /* eslint-disable jsx-a11y/no-noninteractive-element-interactions,
                        jsx-a11y/no-noninteractive-tabindex */
      <pre
        className="cm-scroller"
        data-testid="code-pane"
        data-path={path}
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === '/') {
            e.preventDefault();
            paneMocks.slashOpen.code?.();
          }
        }}
      >
        {text}
      </pre>
      /* eslint-enable jsx-a11y/no-noninteractive-element-interactions,
                        jsx-a11y/no-noninteractive-tabindex */
    );
  },
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

vi.mock('./file-viewer-markdown', async () => {
  const actual =
    await vi.importActual<typeof import('./file-viewer-markdown')>(
      './file-viewer-markdown',
    );
  return {
    ...actual,
    // The real MarkdownPane pulls react-markdown, which is fine in jsdom, but
    // we stub it to keep the assertion surface simple and avoid coupling test
    // expectations to remark output.
    MarkdownPane: ({
      path,
      text,
      onSearchAdapterReady,
      onSearchCount,
      onSlashOpen,
    }: {
      path: string;
      text: string;
      onSearchAdapterReady?: (adapter: PaneSearchAdapter | null) => void;
      onSearchCount?: (current: number, total: number) => void;
      onSlashOpen?: () => void;
    }) => {
      reactUseEffect(() => {
        paneMocks.slashOpen.markdown = onSlashOpen ?? null;
        const adapter = makeFakeAdapter('markdown', onSearchCount);
        paneMocks.adapters.markdown = adapter;
        onSearchAdapterReady?.(adapter);
        return () => {
          onSearchAdapterReady?.(null);
          paneMocks.adapters.markdown = null;
          paneMocks.slashOpen.markdown = null;
        };
        // eslint-disable-next-line react-hooks/exhaustive-deps
      }, [path, text]);
      return (
        /* eslint-disable jsx-a11y/no-static-element-interactions,
                          jsx-a11y/no-noninteractive-tabindex */
        <div
          className="file-viewer-markdown-body calm-prose"
          data-testid="markdown-pane"
          data-path={path}
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === '/') {
              e.preventDefault();
              paneMocks.slashOpen.markdown?.();
            }
          }}
        >
          {text}
        </div>
        /* eslint-enable jsx-a11y/no-static-element-interactions,
                          jsx-a11y/no-noninteractive-tabindex */
      );
    },
  };
});

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
  const [slot] = useCardInstanceCtx().useCardSlot<{
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
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
    registerCard(FileViewerEntry);
    mocks.dlog.mockClear();
    overlayStore.clear();
    paneMocks.reset();
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
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
  });

  it('logs visibility hints through the lifecycle writer', async () => {
    render(
      <CardInstanceProvider
        cardId="file_1"
        deletable
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    await waitFor(() =>
      expect(resolveCardById('file_1')?.writer).toBeDefined(),
    );
    act(() => {
      resolveCardById('file_1')!.writer.setVisible(false);
    });

    expect(mocks.dlog).toHaveBeenCalledWith('FileViewerCard', 'visibility', {
      cardId: 'file_1',
      visible: false,
    });
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

  it('renders .md files as a Markdown preview by default, with a Source toggle back to CodeMirror', async () => {
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/README.md') {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo',
        parent: '/',
        entries: [{ name: 'README.md', is_dir: false }],
      };
    });
    vi.mocked(api.readFile).mockResolvedValue({
      path: '/repo/README.md',
      size: 40,
      text: '# Hello\n\nA line of text.\n',
      truncated: false,
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/README.md' }}
      />,
    );

    const md = await screen.findByTestId('markdown-pane');
    expect(md).toHaveAttribute('data-path', '/repo/README.md');
    expect(md).toHaveTextContent('# Hello');
    expect(screen.queryByTestId('code-pane')).toBeNull();

    fireEvent.click(screen.getByRole('tab', { name: 'Source' }));
    expect(await screen.findByTestId('code-pane')).toHaveAttribute(
      'data-path',
      '/repo/README.md',
    );
    expect(screen.queryByTestId('markdown-pane')).toBeNull();

    fireEvent.click(screen.getByRole('tab', { name: 'Preview' }));
    expect(await screen.findByTestId('markdown-pane')).toBeTruthy();
  });

  it('does not show the Markdown mode toggle for non-markdown files', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    await screen.findByTestId('code-pane');
    expect(screen.queryByRole('tab', { name: 'Preview' })).toBeNull();
    expect(screen.queryByRole('tab', { name: 'Source' })).toBeNull();
    expect(screen.queryByTestId('markdown-pane')).toBeNull();
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

describe('FileViewerCard `/` search bar', () => {
  const overlayStore = new Map<string, KernelOverlay>();

  beforeEach(() => {
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
    registerCard(FileViewerEntry);
    overlayStore.clear();
    paneMocks.reset();
    try {
      window.localStorage.clear();
    } catch {
      // ignore
    }
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/src/main.ts' || path === '/repo/README.md') {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo/src',
        parent: '/repo',
        entries: [{ name: 'main.ts', is_dir: false }],
      };
    });
    vi.mocked(api.readFile).mockImplementation(async (path: string) => ({
      path,
      size: 20,
      text: `contents of ${path}`,
      truncated: false,
    }));
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
    vi.mocked(api.upsertOverlay).mockImplementation(async (body: NewOverlayBody) => {
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
    });
  });

  afterEach(() => {
    vi.clearAllMocks();
    __resetRegistryForTest();
    __resetCardEntryResolverRegistryForTest();
  });

  it('opens the bar when `/` is pressed in the code pane', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    const code = await screen.findByTestId('code-pane');
    expect(screen.queryByRole('search')).toBeNull();

    fireEvent.keyDown(code, { key: '/' });

    const bar = await screen.findByRole('search');
    expect(bar).toBeTruthy();
    const input = screen.getByLabelText('Search in file');
    expect(input).toBeTruthy();
  });

  it('opens the bar when `/` is pressed in the markdown pane', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/README.md' }}
      />,
    );

    const md = await screen.findByTestId('markdown-pane');
    expect(screen.queryByRole('search')).toBeNull();

    fireEvent.keyDown(md, { key: '/' });
    expect(await screen.findByRole('search')).toBeTruthy();
  });

  it('closes the bar and clears highlights on Esc', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    fireEvent.keyDown(await screen.findByTestId('code-pane'), { key: '/' });
    const input = (await screen.findByLabelText('Search in file')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'foo' } });
    await waitFor(() => expect(paneMocks.setQuerySpies.code).toHaveBeenCalledWith('foo'));

    fireEvent.keyDown(input, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('search')).toBeNull());
    // On close, adapter is asked to clear its query (setQuery('')).
    expect(paneMocks.setQuerySpies.code).toHaveBeenLastCalledWith('');
  });

  it('routes Enter → next and Shift+Enter → prev while input is focused', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    fireEvent.keyDown(await screen.findByTestId('code-pane'), { key: '/' });
    const input = (await screen.findByLabelText('Search in file')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'hi' } });

    fireEvent.keyDown(input, { key: 'Enter' });
    expect(paneMocks.nextSpies.code).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(input, { key: 'Enter', shiftKey: true });
    expect(paneMocks.prevSpies.code).toHaveBeenCalledTimes(1);
  });

  it('shows no match for a first search with zero matches', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    fireEvent.keyDown(await screen.findByTestId('code-pane'), { key: '/' });
    const input = (await screen.findByLabelText('Search in file')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'missing' } });

    expect(await screen.findByText('no match')).toBeTruthy();
  });

  it('closes the bar when the file path changes', async () => {
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (
        path === '/repo/src/main.ts' ||
        path === '/repo/src/other.ts'
      ) {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo/src',
        parent: '/repo',
        entries: [
          { name: 'main.ts', is_dir: false },
          { name: 'other.ts', is_dir: false },
        ],
      };
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    fireEvent.keyDown(await screen.findByTestId('code-pane'), { key: '/' });
    expect(await screen.findByRole('search')).toBeTruthy();

    fireEvent.click(await screen.findByText('other.ts'));
    await waitFor(() => expect(screen.queryByRole('search')).toBeNull());
  });

  it('does not open the bar for image files (no active pane)', async () => {
    vi.mocked(api.listDir).mockImplementation(async (path?: string) => {
      if (path === '/repo/pic.png') {
        throw new api.CalmApiError(400, 'bad_request', 'not a directory');
      }
      return {
        path: '/repo',
        parent: '/',
        entries: [{ name: 'pic.png', is_dir: false }],
      };
    });

    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/pic.png' }}
      />,
    );

    await screen.findByRole('img', { name: '/repo/pic.png' });
    // No pane means no `/` handler; the bar cannot be opened.
    expect(screen.queryByRole('search')).toBeNull();
    expect(paneMocks.slashOpen.code).toBeNull();
    expect(paneMocks.slashOpen.markdown).toBeNull();
  });

  it('does not open the bar when the diff tab is active', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/src/main.ts' }}
      />,
    );

    await screen.findByTestId('code-pane');
    fireEvent.click(screen.getByRole('tab', { name: 'Diff' }));
    await screen.findByTestId('diff-pane');
    // Diff pane has no `/` handler at all.
    expect(paneMocks.slashOpen.code).toBeNull();
    expect(paneMocks.slashOpen.markdown).toBeNull();
    expect(screen.queryByRole('search')).toBeNull();
  });

  it('renders the markdown pane with the calm-prose typography class', async () => {
    const Component = FileViewerEntry.Component;
    renderWithClient(
      <Component
        card={{ type: 'file-viewer', id: 'file_1', path: '/repo/README.md' }}
      />,
    );
    const md = await screen.findByTestId('markdown-pane');
    expect(md).toHaveClass('calm-prose');
    expect(md).toHaveClass('file-viewer-markdown-body');
  });
});

describe('createMarkdownSearchAdapter', () => {
  it('no-ops gracefully when CSS.highlights is undefined', async () => {
    const { createMarkdownSearchAdapter } =
      await vi.importActual<typeof import('./file-viewer-markdown')>(
        './file-viewer-markdown',
      );

    // jsdom does not implement CSS.highlights out of the box — assert the
    // adapter builds, count callback fires with zeros, and next/prev do
    // not crash even when Highlight is absent.
    const container = document.createElement('div');
    container.innerHTML = '<p>hello world</p>';
    document.body.appendChild(container);
    const counts: Array<[number, number]> = [];
    const adapter = createMarkdownSearchAdapter(container, (cur, total) => {
      counts.push([cur, total]);
    });
    expect(() => adapter.setQuery('world')).not.toThrow();
    expect(() => adapter.next()).not.toThrow();
    expect(() => adapter.prev()).not.toThrow();
    expect(() => adapter.dispose()).not.toThrow();
    // At minimum, setQuery pushed a zero count in the fallback path.
    expect(counts.length).toBeGreaterThan(0);
    document.body.removeChild(container);
  });
});
