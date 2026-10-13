import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { trackWorkspaceRawFileUrl } from '../../../../../core/domain/fs.ts';
import { ReportDocument } from './public.tsx';

afterEach(cleanup);

it.each(['png', 'svg'])('renders a workspace %s inline through the Track file port', async (extension) => {
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
