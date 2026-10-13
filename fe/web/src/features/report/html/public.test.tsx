// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WorkspaceFilePort } from '../../../../../core/domain/fs.ts';
import { ApiError } from '../../../../../core/domain/failure-class.ts';
import type { ReportBlock } from '../../../../../core/domain/report.ts';
import { ReportDocument } from '../document/public.tsx';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const PATH = 'docs/architecture/overview.html';
const RAW = `/api/tracks/t1/workspace/readfile-raw?path=${encodeURIComponent(PATH)}`;
const PAGE = '<!doctype html><svg></svg>';

function files(readFile: WorkspaceFilePort['readFile']): WorkspaceFilePort {
  return { readFile, rawUrl: (path) => `/api/tracks/t1/workspace/readfile-raw?path=${encodeURIComponent(path)}` };
}

const readable = () => files(vi.fn(() => Promise.resolve({ path: PATH, size: PAGE.length, text: PAGE, truncated: false })));

function htmlBlock(payload: Extract<ReportBlock, { kind: 'html' }>['payload']): ReportBlock {
  return { id: 'b-html', kind: 'html', payload };
}

/** Through the document, the block's one production entry; the Track file port arrives on `linkPreview`, as for prose images. */
function renderBlock(payload: Extract<ReportBlock, { kind: 'html' }>['payload'], port?: WorkspaceFilePort,
  onOpenFileLink?: (target: { path: string }) => void) {
  const report = { summary: '', body: '', blocks: [htmlBlock(payload)] };
  return render(<ReportDocument report={report} empty={null}
    linkPreview={port === undefined ? undefined : { files: port, trackId: 't1', report }} onOpenFileLink={onOpenFileLink} />);
}

describe('ReportHtmlBlock', () => {
  it('frames the Track raw URL with scripts only, no referrer and lazy loading, after the page reads', async () => {
    const port = readable();
    const { container } = renderBlock({ path: PATH, title: 'Architecture', height: 640 }, port);
    const frame = await screen.findByTitle('Architecture');
    expect(frame.tagName).toBe('IFRAME');
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
    expect(frame.getAttribute('referrerpolicy')).toBe('no-referrer');
    expect(frame.getAttribute('loading')).toBe('lazy');
    expect(frame.getAttribute('src')).toBe(RAW);
    expect(frame.style.blockSize).toBe('640px');
    expect(port.readFile).toHaveBeenCalledWith(PATH);
    expect(container.querySelectorAll('iframe')).toHaveLength(1);
  });

  it('defaults the frame to 480 px and names it by its path without a title', async () => {
    renderBlock({ path: PATH }, readable());
    expect((await screen.findByTitle(PATH)).style.blockSize).toBe('480px');
  });

  it('shows the read failure and mounts no frame when the page is missing', async () => {
    const readFile = vi.fn(() => Promise.reject(new ApiError({
      kind: 'http', status: 404, code: 'path_not_found', message: `path ${PATH} not found`,
    })));
    const { container } = renderBlock({ path: PATH }, files(readFile));
    expect((await screen.findByRole('alert')).textContent).toContain('File or folder not found.');
    expect(container.querySelector('iframe')).toBeNull();
  });

  it('mounts no frame while the page is still being read', () => {
    const { container } = renderBlock({ path: PATH }, files(() => new Promise(() => {})));
    expect(screen.getByRole('status').textContent).toBe('Loading page…');
    expect(container.querySelector('iframe')).toBeNull();
  });

  it('mounts no frame for a page over the read cap, which the raw route refuses', async () => {
    const readFile = vi.fn(() => Promise.resolve({ path: PATH, size: 3 * 1024 * 1024, text: 'a', truncated: true }));
    const { container } = renderBlock({ path: PATH }, files(readFile));
    expect((await screen.findByRole('note')).textContent).toContain('larger than 2 MiB');
    expect(container.querySelector('iframe')).toBeNull();
  });

  it('shows the desktop note without reading the page or framing it in the mobile app, whose document policy blocks frames', async () => {
    vi.stubGlobal('__NC_BUNDLED__', true);
    const port = readable();
    const { container } = renderBlock({ path: PATH, title: 'Architecture' }, port);
    expect((await screen.findByRole('note')).textContent).toContain('Open this page on desktop');
    expect(container.querySelector('iframe')).toBeNull();
    expect(port.readFile).not.toHaveBeenCalled();
    expect(container.querySelector('figcaption')?.textContent).toContain('Architecture');
  });

  it('says the view carries no workspace and mounts no frame without linkPreview', () => {
    const { container } = renderBlock({ path: PATH });
    expect(screen.getByRole('note').textContent).toBe('This view does not carry workspace files.');
    expect(container.querySelector('iframe')).toBeNull();
  });

  it('opens the page full size through the file link handler', async () => {
    const onOpenFileLink = vi.fn();
    renderBlock({ path: PATH }, readable(), onOpenFileLink);
    await userEvent.click(screen.getByRole('button', { name: `Open ${PATH}` }));
    expect(onOpenFileLink).toHaveBeenCalledWith({ path: PATH });
  });
});
