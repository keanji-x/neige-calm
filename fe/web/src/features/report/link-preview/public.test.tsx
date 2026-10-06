import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { trackReportLinkUrl, type TrackReport } from '../../../../../core/domain/report.ts';
import { ReportDocument } from '../document/public.tsx';
import { externalPreviewUrl } from './public.tsx';

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });
const files: WorkspaceFilePort = {
  readFile: vi.fn((path: string) => Promise.resolve({ path, text: '# File contents\n\nRead [sibling](./other.txt).', size: 60, truncated: false })),
  rawUrl: vi.fn((path) => `/workspace/${path}`),
};
function mount(body: string) {
  const report: TrackReport = { summary: '', body, blocks: null };
  return render(<ReportDocument report={report} empty={null} fileRoot="/repo"
    linkPreview={{ files, trackId: 't1', report }} onOpenFileLink={vi.fn()} />);
}
async function hover(name: string) {
  fireEvent.pointerEnter(screen.getByRole('button', { name }).parentElement!);
  await act(async () => { await vi.advanceTimersByTimeAsync(300); });
}

describe('report preview admission and rendering', () => {
  it('renders workspace Markdown only after hover, and resolves nested files from its directory', async () => {
    vi.mocked(files.readFile).mockClear();
    mount('[notes](./docs/notes.md)');
    expect(files.readFile).not.toHaveBeenCalled();
    await hover('notes');
    expect(files.readFile).toHaveBeenCalledExactlyOnceWith('docs/notes.md');
    expect(screen.getByRole('heading', { name: 'File contents' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'sibling' }).title).toBe('docs/other.txt');
  });
  it('never loads an external resource on hover or continued hovering', async () => {
    mount('![diagram](https://example.com/diagram.png)');
    await hover('diagram');
    act(() => { vi.advanceTimersByTime(2000); });
    expect(document.querySelectorAll('img, iframe')).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'Load image' }));
    const image = screen.getByRole('img', { name: 'diagram' });
    expect(image.getAttribute('src')).toBe('https://example.com/diagram.png');
    expect(image.getAttribute('referrerpolicy')).toBe('no-referrer');
  });
  it('loads webpages only on explicit activation in a sandbox without app origin privileges', async () => {
    mount('[site](https://example.com)');
    await hover('site');
    expect(document.querySelector('iframe')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Load webpage' }));
    const frame = document.querySelector('iframe')!;
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame.getAttribute('referrerpolicy')).toBe('no-referrer');
    expect(screen.getByRole('link', { name: /Open in new tab/ }).getAttribute('rel')).toBe('noopener noreferrer');
  });
  it('rejects unsafe schemes, credentials and root escapes for links and images', () => {
    mount('[script](javascript:alert) [data](data:text/html,evil) [secret](https://user:password@example.com) [outside](../secret.txt) ![outside image](../secret.png)');
    expect(screen.queryAllByRole('button')).toHaveLength(0);
    expect(document.querySelectorAll('img, iframe, a')).toHaveLength(0);
  });
  it('previews an internal prose reference through the existing Markdown renderer', async () => {
    const report: TrackReport = { summary: '', body: '', blocks: [
      { id: 'b_1', kind: 'prose', payload: { markdown: `[details](${trackReportLinkUrl('t1')}#b_2)` } },
      { id: 'b_2', kind: 'prose', payload: { markdown: '## Detail\n\nThe actual content.' } },
    ] };
    render(<ReportDocument report={report} empty={null} linkPreview={{ files, trackId: 't1', report }} onOpenLink={vi.fn()} />);
    await hover('details');
    expect(within(screen.getByRole('dialog')).getByText('The actual content.')).toBeTruthy();
  });
  it('resets the source directory across file to report to file previews', async () => {
    const report: TrackReport = { summary: '', body: '', blocks: [
      { id: 'b_1', kind: 'prose', payload: { markdown: '[notes](./docs/notes.md)' } },
      { id: 'b_2', kind: 'prose', payload: { markdown: '[root asset](./assets/chart.png)' } },
    ] };
    vi.mocked(files.readFile).mockResolvedValueOnce({ path: 'docs/notes.md', size: 40, truncated: false,
      text: `[reference](${trackReportLinkUrl('t1')}#b_2)` });
    render(<ReportDocument report={report} empty={null} fileRoot="/repo" linkPreview={{ files, trackId: 't1', report }}
      onOpenFileLink={vi.fn()} onOpenLink={vi.fn()} />);
    await hover('notes');
    await hover('reference');
    expect(within(screen.getByRole('dialog', { name: 'Preview: reference' })).getByRole('button', { name: 'root asset' }).title).toBe('assets/chart.png');
  });
  it('allows report reference previews when the host has no navigation callback', async () => {
    const report: TrackReport = { summary: '', body: '', blocks: [
      { id: 'b_1', kind: 'prose', payload: { markdown: `[reference](${trackReportLinkUrl('t1')}#b_2)` } },
      { id: 'b_2', kind: 'prose', payload: { markdown: 'Report-owned content.' } },
    ] };
    render(<ReportDocument report={report} empty={null} linkPreview={{ files, trackId: 't1', report }} />);
    await hover('reference');
    expect(within(screen.getByRole('dialog')).getByText('Report-owned content.')).toBeTruthy();
  });
  it('keeps an image inside a link as a single accessible preview trigger', async () => {
    mount('[![linked image](./assets/chart.png)](https://example.com)');
    expect(document.querySelector('button button')).toBeNull();
    await hover('linked image');
    expect(screen.getByRole('dialog', { name: 'Preview: linked image' })).toBeTruthy();
  });
  it('uses the scoped image port for workspace image previews', async () => {
    mount('![local image](./assets/chart.png)');
    await hover('local image');
    expect(screen.getByRole('img').getAttribute('src')).toBe('/workspace/assets/chart.png');
  });
});

describe('external URL contract', () => {
  it.each(['javascript:alert(1)', 'data:text/html,x', '//example.com', '/api/logout', 'https://u:p@example.com', 'https://example.com\\evil', 'https://example.com\n', 'file:///etc/passwd'])('refuses %s', (url) => {
    expect(externalPreviewUrl(url)).toBeNull();
  });
  it('admits an explicit HTTP(S) destination', () => { expect(externalPreviewUrl('https://example.com/path')).toBe('https://example.com/path'); });
});
