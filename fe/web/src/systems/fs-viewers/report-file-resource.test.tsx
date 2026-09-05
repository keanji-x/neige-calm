// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { useReportFileResource } from './public.tsx';

afterEach(cleanup);

function Probe({ path, files, onOpened }: {
  path: string;
  files: WorkspaceFilePort;
  onOpened?: (path: string) => void;
}) {
  const resource = useReportFileResource(path, files, onOpened);
  return <div data-kind={resource.kind} data-format={resource.kind === 'loaded' ? resource.format : ''}>
    {resource.kind === 'loaded' ? resource.text : resource.kind}
  </div>;
}

describe('useReportFileResource', () => {
  it('classifies and loads Markdown through the injected workspace port', async () => {
    const readFile = vi.fn(() => Promise.resolve({
      path: 'docs/guide.md', size: 7, text: '# Guide', truncated: false,
    }));
    const onOpened = vi.fn();
    render(<Probe
      path="docs/guide.md"
      files={{ readFile, rawUrl: (path) => `/raw/${path}` }}
      onOpened={onOpened}
    />);

    const loaded = await screen.findByText('# Guide');
    expect(loaded.dataset.format).toBe('markdown');
    expect(readFile).toHaveBeenCalledWith('docs/guide.md');
    expect(onOpened).toHaveBeenCalledWith('docs/guide.md');
  });

  it('does not let a stale read replace the new path', async () => {
    let resolveFirst!: (value: { path: string; size: number; text: string; truncated: boolean }) => void;
    const readFile = vi.fn((path: string) => path === 'a.txt'
      ? new Promise<ReturnType<WorkspaceFilePort['readFile']> extends Promise<infer Value> ? Value : never>(
          (resolve) => { resolveFirst = resolve; },
        )
      : Promise.resolve({ path, size: 1, text: 'second', truncated: false }));
    const files = { readFile, rawUrl: (path: string) => `/raw/${path}` };
    const view = render(<Probe path="a.txt" files={files} />);
    view.rerender(<Probe path="b.txt" files={files} />);
    expect(await screen.findByText('second')).toBeTruthy();

    resolveFirst({ path: 'a.txt', size: 1, text: 'first', truncated: false });
    await waitFor(() => { expect(screen.queryByText('first')).toBeNull(); });
  });
});
