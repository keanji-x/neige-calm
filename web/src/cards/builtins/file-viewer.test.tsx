// File-viewer card tests. CodeMirror itself is lazy-loaded and mocked here;
// these tests pin the kernel adapter and the card's data-fetching/render
// contract without pulling editor packages into jsdom.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import type { KernelCard } from '../../api/wire';

vi.mock('../../app/theme', () => ({
  useTheme: () => ({ resolved: 'light' }),
}));

vi.mock('./file-viewer-codemirror', () => ({
  CodePane: ({ path, text }: { path: string; text: string }) => (
    <pre data-testid="code-pane" data-path={path}>
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
    <pre data-testid="diff-pane" data-path={path}>
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
  };
});

import { FileViewerEntry } from './file-viewer';
import * as api from '../../api/calm';

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
  beforeEach(() => {
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
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it('loads the parent directory for an initial file path and renders read-only code', async () => {
    const Component = FileViewerEntry.Component;
    render(
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

  it('collapses and expands the file tree from the toolbar', async () => {
    const Component = FileViewerEntry.Component;
    render(
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
});
