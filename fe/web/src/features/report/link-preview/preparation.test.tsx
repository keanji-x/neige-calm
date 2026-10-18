import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import * as markdown from '../../../../../core/markdown/public.ts';
import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { ReportFileViewer } from '../file-viewer/public.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const FILE_TEXT = '# File contents\n\n[Repository](https://github.com/example/project/issues/1), issue #2, [sibling](./other.md) and [private](/repo/private.md).';
function filePort(text = FILE_TEXT): WorkspaceFilePort {
  return { readFile: vi.fn<WorkspaceFilePort['readFile']>(path => Promise.resolve({ path, text, size: text.length, truncated: false })),
    rawUrl: path => `/files/${path}` };
}
it('prepares a loaded Markdown file once across parent updates while keeping current actions', async () => {
  const parse = vi.spyOn(markdown, 'parse');
  const files = filePort(); const previous = vi.fn(); const current = vi.fn();
  const view = render(<ReportFileViewer path="docs/notes.md" files={files} fileRoot="/repo" wide
    onClose={() => {}} onOpenFileLink={previous} />);
  await screen.findByRole('heading', { name: 'File contents' });
  expect(parse.mock.calls.filter(([source]) => source === FILE_TEXT)).toHaveLength(1);
  const paragraph = screen.getByRole('button', { name: 'sibling' }).closest('p');
  expect(screen.getByRole('button', { name: 'private' })).toBeTruthy();
  parse.mockClear();
  view.rerender(<ReportFileViewer path="docs/notes.md" files={files} fileRoot="/other" wide={false}
    onClose={() => {}} onOpenFileLink={current} />);
  expect(parse).not.toHaveBeenCalled();
  expect(screen.queryByRole('button', { name: 'private' })).toBeNull();
  expect(screen.getByRole('button', { name: 'sibling' }).closest('p')).toBe(paragraph);
  fireEvent.click(screen.getByRole('button', { name: 'sibling' }));
  expect(previous).not.toHaveBeenCalled();
  expect(current).toHaveBeenCalledWith({ path: 'docs/other.md' });
  expect(files.readFile).toHaveBeenCalledTimes(1);

  const nextText = '# Updated file\n\nNew content.';
  const replacement = filePort(nextText);
  view.rerender(<ReportFileViewer path="docs/notes.md" files={replacement} fileRoot="/repo" wide onClose={() => {}} />);
  await screen.findByRole('heading', { name: 'Updated file' });
  expect(screen.queryByRole('heading', { name: 'File contents' })).toBeNull();
  expect(parse.mock.calls.filter(([source]) => source === nextText)).toHaveLength(1);
});
