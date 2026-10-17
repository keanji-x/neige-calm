import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import * as markdown from '../../../../../core/markdown/public.ts';
import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { trackReportLinkUrl, type TrackReport } from '../../../../../core/domain/report.ts';
import { ApiError } from '../../../../../core/domain/failure-class.ts';
import { ReportDocument } from '../document/public.tsx';
import { ReportFileViewer } from '../file-viewer/public.tsx';

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const FILE_TEXT = '# File contents\n\n[Repository](https://github.com/example/project/issues/1), issue #2, [sibling](./other.md) and [private](/repo/private.md).';
const report: TrackReport = { summary: '', body: '[notes](./docs/notes.md)', blocks: null };
function filePort(text = FILE_TEXT): WorkspaceFilePort {
  return { readFile: vi.fn<WorkspaceFilePort['readFile']>(path => Promise.resolve({ path, text, size: text.length, truncated: false })),
    rawUrl: path => `/files/${path}` };
}
function openPreview(name: string) {
  fireEvent.keyDown(screen.getByRole('button', { name }), { key: 'ArrowDown' });
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

it('shares file preview context and prose parsing without preparing a closed preview', async () => {
  const parse = vi.spyOn(markdown, 'parse'); const files = filePort();
  const previous = vi.fn(); const current = vi.fn();
  const props = { report, empty: null, fileRoot: '/repo', linkPreview: { files, trackId: 'track', report } };
  const view = render(<ReportDocument {...props} onOpenFileLink={previous} />);
  expect(files.readFile).not.toHaveBeenCalled();
  expect(parse.mock.calls.filter(([source]) => source === FILE_TEXT)).toHaveLength(0);
  openPreview('notes');
  const preview = await screen.findByRole('dialog', { name: 'Preview: notes' });
  await within(preview).findByRole('heading', { name: 'File contents' });
  expect(parse.mock.calls.filter(([source]) => source === FILE_TEXT)).toHaveLength(1);
  expect(within(preview).getByRole('button', { name: 'private' })).toBeTruthy();
  parse.mockClear();
  view.rerender(<ReportDocument {...props} fileRoot="/other" onOpenFileLink={current} />);
  expect(parse).not.toHaveBeenCalled();
  expect(within(preview).queryByRole('button', { name: 'private' })).toBeNull();
  fireEvent.click(within(preview).getByRole('button', { name: 'sibling' }));
  expect(previous).not.toHaveBeenCalled();
  expect(current).toHaveBeenCalledWith({ path: 'docs/other.md' });
  await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Preview: notes' })).toBeNull());
  expect(files.readFile).toHaveBeenCalledTimes(1);
});

it('keeps a referenced block in the whole report repository context', async () => {
  const parse = vi.spyOn(markdown, 'parse');
  const text = 'Only issue #2 is mentioned here.';
  const document: TrackReport = { summary: '', body: '', blocks: [
    { id: 'links', kind: 'prose', payload: { markdown: `[details](${trackReportLinkUrl('track')}#detail) and [Repository](https://github.com/example/project/issues/1)` } },
    { id: 'detail', kind: 'prose', payload: { markdown: text } },
  ] };
  const files = filePort();
  const view = render(<ReportDocument report={document} empty={null} linkPreview={{ files, trackId: 'track', report: document }} />);
  parse.mockClear();
  openPreview('details');
  const preview = await screen.findByRole('dialog', { name: 'Preview: details' });
  expect(parse.mock.calls.filter(([source]) => source === text)).toHaveLength(1);
  fireEvent.keyDown(within(preview).getByRole('button', { name: 'issue #2' }), { key: 'ArrowDown' });
  const reference = screen.getByRole('dialog', { name: 'Preview: issue #2' });
  expect(within(reference).getByRole('link', { name: 'Open in new tab ↗' }).getAttribute('href')).toBe('https://github.com/example/project/issues/2');
  fireEvent.keyDown(reference, { key: 'Escape' });
  const updated: TrackReport = { ...document, blocks: [
    { id: 'links', kind: 'prose', payload: { markdown: `[details](${trackReportLinkUrl('track')}#detail) and [Repository](https://github.com/other/repository/issues/1)` } },
    { id: 'detail', kind: 'prose', payload: { markdown: text } },
  ] };
  view.rerender(<ReportDocument report={updated} empty={null} linkPreview={{ files, trackId: 'track', report: updated }} />);
  fireEvent.keyDown(within(preview).getByRole('button', { name: 'issue #2' }), { key: 'ArrowDown' });
  expect(within(screen.getByRole('dialog', { name: 'Preview: issue #2' }))
    .getByRole('link', { name: 'Open in new tab ↗' }).getAttribute('href')).toBe('https://github.com/other/repository/issues/2');
});

it('does not parse a late closed file or take focus, and rereads changed content on reopening', async () => {
  const parse = vi.spyOn(markdown, 'parse');
  let release!: (value: Awaited<ReturnType<WorkspaceFilePort['readFile']>>) => void;
  const readFile = vi.fn<WorkspaceFilePort['readFile']>()
    .mockImplementationOnce(() => new Promise(resolve => { release = resolve; }))
    .mockResolvedValue({ path: 'docs/notes.md', text: '# Reopened current content', size: 26, truncated: false });
  const files = { readFile, rawUrl: (path: string) => path };
  render(<><button type="button">Another action</button><ReportDocument report={report} empty={null} fileRoot="/repo"
    linkPreview={{ files, trackId: 'track', report }} /></>);
  openPreview('notes');
  const preview = await screen.findByRole('dialog', { name: 'Preview: notes' });
  expect(within(preview).getByRole('status').textContent).toBe('Loading file…');
  fireEvent.keyDown(preview, { key: 'Escape' });
  const action = screen.getByRole('button', { name: 'Another action' });
  action.focus();
  parse.mockClear();
  await act(async () => {
    release({ path: 'docs/notes.md', text: FILE_TEXT, size: FILE_TEXT.length, truncated: false });
    await Promise.resolve();
  });
  expect(screen.queryByRole('dialog', { name: 'Preview: notes' })).toBeNull();
  expect(parse).not.toHaveBeenCalled();
  expect(document.activeElement).toBe(action);
  openPreview('notes');
  await screen.findByRole('heading', { name: 'Reopened current content' });
  expect(readFile.mock.calls).toEqual([['docs/notes.md'], ['docs/notes.md']]);
  expect(parse.mock.calls.map(([source]) => source)).toEqual(['# Reopened current content']);
  expect(screen.queryByRole('heading', { name: 'File contents' })).toBeNull();
});

it.each([
  [403, 'forbidden', 'Access denied.'],
  [404, 'path_not_found', 'File or folder not found.'],
] as const)('keeps %s file failure and retry with the existing resource owner', async (status, code, sentence) => {
  const parse = vi.spyOn(markdown, 'parse');
  const readFile = vi.fn<WorkspaceFilePort['readFile']>()
    .mockRejectedValueOnce(new ApiError({ kind: 'http', status, code, message: '' }))
    .mockResolvedValue({ path: 'docs/notes.md', text: FILE_TEXT, size: FILE_TEXT.length, truncated: false });
  const files = { readFile, rawUrl: (path: string) => path };
  render(<ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ files, trackId: 'track', report }} />);
  openPreview('notes');
  await screen.findByText(sentence);
  expect(parse.mock.calls.filter(([source]) => source === FILE_TEXT)).toHaveLength(0);
  fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
  await screen.findByRole('heading', { name: 'File contents' });
  expect(parse.mock.calls.filter(([source]) => source === FILE_TEXT)).toHaveLength(1);
  expect(readFile.mock.calls).toEqual([['docs/notes.md'], ['docs/notes.md']]);
});

it('keeps canonical oversized Markdown fail-soft and never treats it as HTML', async () => {
  const source = '<script>untrusted()</script>\n' + 'x'.repeat(2_000_000);
  const parse = vi.spyOn(markdown, 'parse'); const files = filePort(source);
  render(<ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ files, trackId: 'track', report }} />);
  openPreview('notes');
  const preview = await screen.findByRole('dialog', { name: 'Preview: notes' });
  await waitFor(() => expect(preview.querySelector('pre')?.textContent).toBe(source));
  expect(preview.querySelector('script')).toBeNull();
  expect(parse.mock.calls.filter(([text]) => text === source)).toHaveLength(1);
});
