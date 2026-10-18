import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { trackWorkspaceRawFileUrl } from '../../../../../core/domain/fs.ts';
import { ProseBlock } from './content.tsx';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

it.each(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'svg'])('renders a workspace %s inline through the Track file port', async (extension) => {
  const path = `docs/figure.${extension}`;
  const report = { summary: '', body: `![Architecture](${path})`, blocks: null };
  const readFile = vi.fn();
  const rawUrl = vi.fn((file: string) => trackWorkspaceRawFileUrl('image-track', file));
  const openFile = vi.fn();
  const { container } = render(<ReportDocument report={report} empty={null} fileRoot="/repo"
    onOpenFileLink={openFile} linkPreview={{ trackId: 'image-track', report, files: { readFile, rawUrl } }} />);
  const image = await screen.findByRole('img', { name: 'Architecture' });
  expect(image.getAttribute('src')).toBe(trackWorkspaceRawFileUrl('image-track', path));
  expect(image.getAttribute('loading')).toBe('lazy');
  expect(container.querySelector('svg, object, iframe')).toBeNull();
  expect(readFile).not.toHaveBeenCalled();
  screen.getByRole('button', { name: 'Architecture' }).click();
  expect(openFile).toHaveBeenCalledWith({ path });
});

function setup(markdown: string, fileBasePath = '') {
  const report = { summary: '', body: markdown, blocks: null };
  const files = { readFile: vi.fn(), rawUrl: vi.fn((path: string) => trackWorkspaceRawFileUrl('image-track', path)) };
  const openFile = vi.fn();
  const resources = { trackId: 'image-track', report, files };
  const rendered = render(<ReportDocument report={report} empty={null} fileRoot="/repo" fileBasePath={fileBasePath}
    onOpenFileLink={openFile} linkPreview={resources} />);
  return { ...rendered, files, openFile, resources };
}

it('normalizes encoded absolute file images and relative Markdown parents', async () => {
  const { files } = setup('![First](file:///repo/docs/a%20b.PNG:12#figure) ![Second](../figure.svg)', 'docs');
  expect((await screen.findByRole('img', { name: 'First' })).getAttribute('src')).toBe(trackWorkspaceRawFileUrl('image-track', 'docs/a b.PNG'));
  expect((await screen.findByRole('img', { name: 'Second' })).getAttribute('src')).toBe(trackWorkspaceRawFileUrl('image-track', 'figure.svg'));
  expect(files.readFile).not.toHaveBeenCalled();
});

it('refuses outside-root and invalid images without requesting resources', () => {
  const { container, files, openFile } = setup('![Escape](/repo-other/secret.png) ![Parent](../../secret.png) ![Encoded](%2Fother%2Fsecret.svg) ![Protocol](//evil.invalid/pic.png) ![Data](data:image/png;base64,AAAA) ![Unsupported](note.txt)', 'docs');
  for (const label of ['Escape', 'Parent', 'Encoded', 'Protocol', 'Data', 'Unsupported']) {
    expect(screen.getByText(`${label} — Image unavailable`)).toBeTruthy();
  }
  expect(container.querySelector('img, object, iframe')).toBeNull();
  expect(files.rawUrl).not.toHaveBeenCalled();
  expect(files.readFile).not.toHaveBeenCalled();
  expect(openFile).not.toHaveBeenCalled();
});

it('replaces missing or unreadable images with visible alt and unavailable', async () => {
  const { container } = setup('![Missing](missing.png) ![Denied](denied.svg)');
  fireEvent.error(await screen.findByRole('img', { name: 'Missing' }));
  fireEvent.error(await screen.findByRole('img', { name: 'Denied' }));
  expect(screen.getByText('Missing — Image unavailable')).toBeTruthy();
  expect(screen.getByText('Denied — Image unavailable')).toBeTruthy();
  expect(container.querySelector('img')).toBeNull();
});

it('keeps the outer link destination without nested image controls', async () => {
  const { container, openFile } = setup('[![Diagram](diagram.svg)](notes.md)');
  const image = await screen.findByRole('img', { name: 'Diagram' });
  expect(image.closest('button')?.getAttribute('title')).toBe('notes.md');
  expect(container.querySelector('button button, button a, a button')).toBeNull();
  fireEvent.click(image);
  expect(openFile).toHaveBeenCalledWith({ path: 'notes.md' });
});

it('never loads or activates images in inert captured evidence', () => {
  const report = { summary: '', body: '', blocks: null };
  const files = { readFile: vi.fn(), rawUrl: vi.fn() };
  const { container } = render(<ProseBlock markdown='![Local](diagram.svg) [![Remote](https://example.com/pic.png)](notes.md)'
    blockId={null} destinationMode="inert" fileRoot="/repo" linkPreview={{ trackId: 'track', report, files }} />);
  expect(container.textContent).toBe('Local Remote');
  expect(container.querySelector('img, iframe, object, button, a')).toBeNull();
  expect(files.rawUrl).not.toHaveBeenCalled();
  expect(files.readFile).not.toHaveBeenCalled();
});

it('shows unavailable when no workspace resource port is supplied', () => {
  render(<ReportDocument report={{ summary: '', body: '![Unavailable](image.png)', blocks: null }} empty={null} fileRoot="/repo" />);
  expect(screen.getByText('Unavailable — Image unavailable')).toBeTruthy();
});
